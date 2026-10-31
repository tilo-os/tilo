//! The capture studio over the session: its actions are the bar's keys,
//! and what it takes is the compositor's own frames.

use super::desk::capture;
use super::*;

/// The capture studio's published resource: its actions are the bar's keys.
const STUDIO: &str = tilo_capture_studio::RESOURCE;

/// A session with the capture studio over it, keeping what it captures in
/// `pictures`.
fn studio_session(pictures: &std::path::Path) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            pictures: Some(pictures.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// Invoke one of the studio's actions with `arguments`, as the key that
/// takes a screenshot does.
#[track_caller]
fn ask_studio(session: &mut Session, action: &str, arguments: &serde_json::Value) {
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    session
        .invoke(STUDIO, action, &arguments)
        .unwrap_or_else(|error| panic!("{STUDIO} {action}: {error:?}"));
}

/// The studio's resource, as the session publishes it behind its door.
fn studio_resource(session: &Session) -> Option<ResourceSpec> {
    fn find(resources: &[ResourceSpec]) -> Option<ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.kind == STUDIO {
                Some(resource.clone())
            } else {
                find(&resource.children)
            }
        })
    }
    find(&session.resources())
}

/// The files under `directory` ending in `extension`, in name order.
fn kept(directory: &std::path::Path, extension: &str) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|end| end == extension))
        .collect();
    paths.sort();
    paths
}

/// A pixel of a frame the compositor read back, as R, G, B, A.
fn shown_at(frame: &tilo_compositor::Screenshot, x: i32, y: i32) -> [u8; 4] {
    let at = (usize::try_from(y).unwrap() * usize::try_from(frame.width).unwrap()
        + usize::try_from(x).unwrap())
        * 4;
    [
        frame.rgba[at],
        frame.rgba[at + 1],
        frame.rgba[at + 2],
        frame.rgba[at + 3],
    ]
}

/// Step the session for `time`, so the recorder is fed.
fn run_for(session: &mut Session, time: Duration) {
    let until = std::time::Instant::now() + time;
    while std::time::Instant::now() < until {
        session.step(Duration::from_millis(5)).expect("step");
    }
}

#[test]
fn the_studio_takes_a_screenshot_of_the_display_as_the_compositor_draws_it() {
    let _settled = ui::motion::testing::settled();
    let pictures = tempfile::tempdir().unwrap();
    let mut session = studio_session(pictures.path());
    settle(&mut session, 4);

    // The bar is down, and its actions are offered anyway: the key takes
    // the screenshot without the studio coming up.
    let resource = studio_resource(&session).expect("the studio publishes its actions");
    let actions: Vec<&str> = resource
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert!(
        actions.contains(&"screenshot") && actions.contains(&"record"),
        "the studio publishes what it takes: {actions:?}"
    );

    ask_studio(
        &mut session,
        "screenshot",
        &serde_json::json!({ "source": "display" }),
    );
    pump(&mut session, &mut [], |_, _| {
        !kept(pictures.path(), "png").is_empty()
    });
    let path = kept(pictures.path(), "png").remove(0);
    let picture = image::open(&path).expect("a picture").to_rgba8();
    assert_eq!(
        picture.dimensions(),
        (
            u32::try_from(WIDTH).unwrap(),
            u32::try_from(HEIGHT).unwrap()
        ),
        "the whole display, at its size"
    );
    // What it kept is what the compositor shows, not a second rendering.
    let shown = session.compositor().screenshot("HEADLESS-1").expect("shot");
    assert_eq!(
        pixel_at(&picture, 20, HEIGHT - 20),
        shown_at(&shown, 20, HEIGHT - 20),
        "the studio reads the compositor's own frames"
    );
}

#[test]
fn the_print_key_takes_a_screenshot_without_the_studio_coming_up() {
    let _settled = ui::motion::testing::settled();
    let pictures = tempfile::tempdir().unwrap();
    let mut session = studio_session(pictures.path());
    settle(&mut session, 4);
    let print = session
        .compositor()
        .keycode_for_keysym(0xff61) // `XK_Print`
        .expect("the layout has a Print key");
    assert!(kept(pictures.path(), "png").is_empty(), "nothing yet");

    tap(&mut session, print);
    pump(&mut session, &mut [], |_, _| {
        !kept(pictures.path(), "png").is_empty()
    });
    let kept = kept(pictures.path(), "png");
    assert_eq!(kept.len(), 1, "one press, one picture: {kept:?}");
    let picture = image::open(&kept[0]).expect("a picture").to_rgba8();
    assert_eq!(
        picture.dimensions(),
        (
            u32::try_from(WIDTH).unwrap(),
            u32::try_from(HEIGHT).unwrap()
        ),
        "the whole display, with the bar still down"
    );
}

#[test]
fn the_studio_takes_a_screenshot_of_one_window_as_its_client_drew_it() {
    let _settled = ui::motion::testing::settled();
    let pictures = tempfile::tempdir().unwrap();
    let mut session = studio_session(pictures.path());
    let _client = mapped(&mut session, "org.example.Terminal");
    let window = session
        .compositor()
        .shareable_windows()
        .into_iter()
        .find(|(_, app_id, _)| app_id.as_deref() == Some("org.example.Terminal"))
        .map(|(identifier, _, _)| identifier)
        .expect("the compositor lists the window");

    ask_studio(
        &mut session,
        "screenshot",
        &serde_json::json!({ "source": "window", "window": window }),
    );
    pump(&mut session, &mut [], |_, _| {
        !kept(pictures.path(), "png").is_empty()
    });
    let path = kept(pictures.path(), "png").remove(0);
    let picture = image::open(&path).expect("a picture").to_rgba8();
    let (width, height) = picture.dimensions();
    assert!(width > 0 && height > 0, "the window has a picture");
    // The window alone: the colour its client drew, not the desk under it.
    #[allow(clippy::cast_possible_wrap, reason = "a window's pixels on a screen")]
    let (middle_x, middle_y) = ((width / 2) as i32, (height / 2) as i32);
    assert_eq!(pixel_at(&picture, middle_x, middle_y), [200, 40, 40, 255]);

    // A capture that names no window takes the one with focus.
    assert!(
        session.compositor().focused_shareable_window().is_some(),
        "the window has focus"
    );
    ask_studio(
        &mut session,
        "screenshot",
        &serde_json::json!({ "source": "window" }),
    );
    pump(&mut session, &mut [], |_, _| {
        kept(pictures.path(), "png").len() == 2
    });
    let path = kept(pictures.path(), "png").remove(1);
    let focused = image::open(&path).expect("a picture").to_rgba8();
    assert_eq!(
        focused.dimensions(),
        (width, height),
        "the focused window, not every window side by side"
    );
}

#[test]
fn a_region_capture_with_no_selection_is_refused() {
    let _settled = ui::motion::testing::settled();
    let pictures = tempfile::tempdir().unwrap();
    let mut session = studio_session(pictures.path());
    settle(&mut session, 4);

    ask_studio(
        &mut session,
        "screenshot",
        &serde_json::json!({ "source": "region" }),
    );
    // A recording of a selection is refused before anything is started: the
    // caller hears why.
    let refused = session.invoke(
        STUDIO,
        "record",
        &serde_json::json!({ "source": "region" })
            .as_object()
            .cloned()
            .unwrap_or_default(),
    );
    assert!(
        refused.is_err(),
        "a region recording needs a selection: {refused:?}"
    );
    run_for(&mut session, Duration::from_millis(100));
    assert!(
        kept(pictures.path(), "png").is_empty(),
        "no screenshot was taken"
    );
    assert!(
        kept(pictures.path(), "mp4").is_empty(),
        "no recording was started"
    );
}

#[test]
fn the_studio_records_the_display_into_a_file_until_it_is_stopped() {
    let _settled = ui::motion::testing::settled();
    // Machines whose media stack cannot encode skip this, as the recorder's
    // own tests do.
    if media::gst::init().is_err()
        || tilo_capture_studio::Recorder::new(tilo_capture_studio::Settings::for_size(
            u32::try_from(WIDTH).unwrap(),
            u32::try_from(HEIGHT).unwrap(),
        ))
        .is_err()
    {
        eprintln!("no encoder on this machine: skipped");
        return;
    }
    let pictures = tempfile::tempdir().unwrap();
    let mut session = studio_session(pictures.path());

    ask_studio(
        &mut session,
        "record",
        &serde_json::json!({ "source": "display" }),
    );
    // The frames are read at the replay buffer's rate.
    run_for(&mut session, Duration::from_millis(400));
    ask_studio(&mut session, "stop-recording", &serde_json::json!({}));
    pump(&mut session, &mut [], |_, _| {
        !kept(pictures.path(), "mp4").is_empty()
    });
    let path = kept(pictures.path(), "mp4").remove(0);
    let file = std::fs::read(&path).expect("the recording is there");
    assert!(!file.is_empty(), "the recording has frames in it");
    // An MP4, as the muxer lays it out: its brand box first.
    assert_eq!(&file[4..8], b"ftyp", "the file is an MP4");
}

#[test]
fn the_studios_open_action_puts_the_bar_over_the_desktop() {
    let _settled = ui::motion::testing::settled();
    let pictures = tempfile::tempdir().unwrap();
    let mut session = studio_session(pictures.path());
    settle(&mut session, 4);
    let desk = session.compositor().screenshot("HEADLESS-1").expect("shot");

    ask_studio(&mut session, "open", &serde_json::json!({}));
    pump(&mut session, &mut [], |session, _| {
        let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
        frame.rgba != desk.rgba
    });
    capture(&mut session, "capture-studio");
}
