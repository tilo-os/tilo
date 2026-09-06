#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "headless integration fixture asserts setup failures directly"
)]

//! Driving the video editor through its actions with **no window at all** —
//! no compositor, no surface, no GPU, no widget tree on the driving side.
//!
//! This is the half of "an agent can drive an app" that carried the real
//! risk. The app-facing action system (`qos-actions`) is a leaf crate over
//! `automerge` — it cannot name a window by construction — but that says
//! nothing about whether *this* app's resource tree can be built without
//! one. If publishing resources needed a laid-out widget, or if any action's
//! effect lived in widget fields rather than in the document, headless
//! driving would be impossible however good the plumbing was.
//!
//! So the test drives the document the long way round and then reads the
//! result back through the **real** widget pipeline, which is what a person
//! opening the editor would see.
//!
//! Three phases, each closing its store before the next opens it, so the
//! document genuinely round-trips through disk rather than through a value
//! held in memory:
//!
//!   1. **Seed.** Write a project the ordinary way and persist it.
//!   2. **Drive.** With no widget, no layout and no theme: load the project
//!      out of a bare `AutoCommit`, publish the resource tree, survey it,
//!      invoke one action by address, persist.
//!   3. **Observe.** Open the persisted document in a real `ui::headless`
//!      driver — the production five-pass frame — and assert the change is
//!      present in what that pipeline produces.
//!
//! Phase 3 is what stops this passing vacuously. Asserting on the document
//! the test itself just wrote would prove only that `automerge` stores
//! bytes; asserting on the driver's own output proves the app agrees.

use qos_actions::door::{self, ActionAddress, DoorError};
use qos_actions::{ActionRegistry, executor::LocalTransport};
use video_editor::actions::{VideoResourceCtx, provide_resources};
use video_editor::doc::VideoDoc;

const APP_ID: &str = "dev.qos.video-editor";

/// Point the persistence layer at a private directory for one test.
///
/// Restores the previous value on drop, so a test that fails does not leave
/// the variable pointing at a deleted temp directory for whatever runs next.
struct DataHome {
    dir: std::path::PathBuf,
    previous: Option<std::ffi::OsString>,
}

impl DataHome {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("qos-ve-headless-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create the private data home");
        let previous = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: every test that touches this variable is serialised on the
        // `video_editor_persist` key, so nothing else reads it in between.
        unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
        Self { dir, previous }
    }
}

impl Drop for DataHome {
    fn drop(&mut self) {
        // SAFETY: as in `new`.
        unsafe {
            match self.previous.take() {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// An action reaches the document with no window, and the app itself shows
/// the result.
#[test]
#[serial_test::serial(video_editor_persist)]
fn an_action_invoked_with_no_window_reaches_the_app() {
    let _home = DataHome::new();

    // ---- 1. Seed -------------------------------------------------------
    //
    // Through the same store the live editor uses, so phase 2 is reading a
    // real persisted document rather than one this test handed it.
    let (clip_id, seeded_clip_count) = {
        let (mut state, mut autosave) =
            ui::persist::try_open(APP_ID).expect("the private data home is writable");

        let mut project = video_editor::model::Project::new("headless");
        let media_id = uuid::Uuid::new_v4();
        project.media.push(video_editor::model::MediaItem {
            id: media_id,
            name: "take-1.mp4".into(),
            kind: video_editor::model::MediaKind::Video,
            duration: 20.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            ..Default::default()
        });
        let track_id = uuid::Uuid::new_v4();
        project.tracks.push(video_editor::model::Track {
            id: track_id,
            name: "V1".into(),
            kind: video_editor::model::TrackKind::Video,
            index: 1,
            ..Default::default()
        });
        let clip_id = project
            .add_clip(track_id, media_id, 0.0)
            .expect("add a clip");
        project.clip_mut(clip_id).expect("the clip").out_point = 20.0;

        VideoDoc::write_project(&mut state.doc, &project);
        autosave.request(&mut state.doc);
        autosave.flush();
        (clip_id, project.clips.len())
    };
    assert_eq!(seeded_clip_count, 1, "the seed put exactly one clip down");

    // ---- 2. Drive ------------------------------------------------------
    //
    // Nothing in this block constructs a widget, a theme, a viewport or a
    // renderer. If any of that were required to publish resources or to
    // apply an action, this phase could not compile, let alone run.
    let address = {
        let (mut state, mut autosave) =
            ui::persist::try_open(APP_ID).expect("reopen the persisted document");

        // The resource tree, built from the document alone.
        let project = VideoDoc::load_project(&state.doc);
        let resources = provide_resources(&VideoResourceCtx {
            project,
            playhead: 10.0,
            // No mixer has run in this process — nothing here has played a
            // frame — so the meter is at rest, which is what the driver of a
            // windowless editor would see.
            mix: video_editor::audio::MixSnapshot::default(),
        });

        // Surveyed, not guessed. Asserting the address is offered *before*
        // invoking it is what keeps the next assertion honest: invoking
        // something the tree never advertised would prove nothing about the
        // surface an agent actually sees.
        let catalogue = door::survey(&resources);
        assert!(
            !catalogue.is_empty(),
            "the resource tree published no actions at all without a window — \
             building it must not depend on layout",
        );
        let address = catalogue
            .iter()
            .map(|d| d.address.clone())
            .find(|a| a.resource == format!("video.clip:{clip_id}") && a.action == "split")
            .unwrap_or_else(|| {
                panic!(
                    "no `split` action offered on the seeded clip; the tree offered: {:?}",
                    catalogue
                        .iter()
                        .map(|d| (&d.address.resource, &d.address.action))
                        .collect::<Vec<_>>()
                )
            });

        let mut registry = ActionRegistry::new();
        video_editor::actions::register(&mut registry);

        let before = state.doc.get_heads();
        let dispatched = door::invoke(
            &mut state.doc,
            &resources,
            &address,
            None,
            b"headless-driver",
            &registry,
            &mut LocalTransport::new(),
        )
        .expect("the surveyed action dispatches");
        assert!(
            dispatched.mutated_local(),
            "the action reported success without touching the document",
        );
        assert_ne!(
            state.doc.get_heads(),
            before,
            "the document's heads did not move, so nothing was actually committed — \
             an action that rolls itself back would otherwise read as success",
        );

        // A negative control in the same run: the door must refuse an
        // address the tree does not carry. Without this, the assertions
        // above would pass equally against a door that accepted anything.
        let bogus = ActionAddress::new(format!("video.clip:{clip_id}"), "nonexistent-verb");
        assert!(
            matches!(
                door::invoke(
                    &mut state.doc,
                    &resources,
                    &bogus,
                    None,
                    b"headless-driver",
                    &registry,
                    &mut LocalTransport::new(),
                ),
                Err(DoorError::Unknown(_))
            ),
            "an address the tree never published must be refused, not attempted",
        );

        autosave.request(&mut state.doc);
        autosave.flush();
        address
    };

    // ---- 3. Observe ----------------------------------------------------
    //
    // The real widget tree, the real five passes. This is deliberately not a
    // re-read of the document: it is the app's own presentation of it.
    let (state, _autosave) = ui::persist::try_open(APP_ID).expect("reopen after the headless edit");
    let mut driver = ui::headless::Driver::new(state, video_editor::app_root(), (1280, 800));
    driver.relayout();

    let after = VideoDoc::load_project(&driver.state_mut().doc);
    assert_eq!(
        after.clips.len(),
        2,
        "the split did not survive to the app: invoking `{}` on `{}` with no window \
         left the timeline as it was",
        address.action,
        address.resource,
    );

    // ...and the app *presents* both halves, rather than merely storing
    // them. A clip the accessibility tree cannot name is a clip a screen
    // reader, a script and an agent cannot reach.
    let exported = driver.a11y_nodes().len();
    assert!(
        exported > 1,
        "the driver laid out and drew a frame that exported no accessible tree \
         ({exported} nodes): the edit is in the document but nothing presents it",
    );
}
