//! The desk's captures: every state of the new session's desk and overview
//! rendered headless, beside the old shell's captures of the same states, and
//! the frames that show the transitions moving.
//!
//! Each state is a test of its own with its own session, so what one leaves
//! open never shows in another's picture. Frames are written to
//! `$VISUAL_DESK_OUT` (default `~/.cache/visual-desk/shots/new`), and the
//! pixel-parity set to `$PIXEL_PARITY_OUT`.

use super::*;

/// Where the parity frames are written.
fn parity_dir() -> std::path::PathBuf {
    std::env::var_os("PIXEL_PARITY_OUT").map_or_else(
        || {
            let home = std::env::var("HOME").expect("HOME");
            std::path::PathBuf::from(home).join(".cache/pixel-parity/new")
        },
        std::path::PathBuf::from,
    )
}

/// Write the screen as `name.png` into [`parity_dir`].
pub(super) fn capture(session: &mut Session, name: &str) {
    let dir = parity_dir();
    std::fs::create_dir_all(&dir).expect("create the parity directory");
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("write the frame");
}

/// Run the session's frame loop on `millis` further than `clock` stands, at
/// the pace a display presents frames, with the clock pinned to each frame's
/// instant: a motion then draws where it stands that far in, the same on
/// every run whatever a frame costs to draw.
fn step_from(session: &mut Session, clock: &mut std::time::Instant, millis: u64) {
    const FRAME: Duration = Duration::from_millis(16);
    let target = *clock + Duration::from_millis(millis);
    while *clock < target {
        *clock = (*clock + FRAME).min(target);
        let at = *clock;
        ui::time::testing::with_monotonic_time(at, || {
            session.step(Duration::from_millis(1)).expect("step");
        });
    }
}

/// Whether the workspace directory sheet is open over the desk.
fn directory_open(session: &Session) -> bool {
    session
        .workspace_directory()
        .is_some_and(|directory| directory.presentation().is_open())
}

/// A written frame's bytes.
fn frame(dir: &std::path::Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join(format!("{name}.png"))).expect("a written frame")
}

/// Write the screen `millis` after `clock` stood, with the frame loop run
/// until then.
fn capture_at(
    session: &mut Session,
    dir: &std::path::Path,
    clock: &mut std::time::Instant,
    millis: u64,
    name: &str,
) {
    step_from(session, clock, millis);
    let at = *clock;
    ui::time::testing::with_monotonic_time(at, || capture_to(session, dir, name));
}

/// Perform the chrome action at `clock`'s instant, so what it starts moves
/// from there.
fn invoke_at(session: &mut Session, clock: std::time::Instant, action: &str) {
    ui::time::testing::with_monotonic_time(clock, || invoke(session, CHROME_RESOURCE, action));
}

/// A session drawing the desk in the light theme.
fn light_session() -> Session {
    fn light() -> ui::application::ComponentRunnerInputs {
        let mut inputs = ui::headless::fixture_runner_inputs();
        inputs.theme = ui::theme::Theme::light();
        inputs
    }
    Session::headless(
        Options {
            presentation: light,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// Run frames until `done` holds, with no client to pump.
#[track_caller]
fn until_session(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    for _ in 0..300 {
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached");
}

/// Hold this test's wall clock at the capture instant, as the launcher holds
/// a capture's: the bar's date and time are then the same on every run, and a
/// frame in which nothing but the real minute turned over is not a change.
/// Only this thread follows it, so the other tests of the process read their
/// own clock.
fn held_clock() -> ui::time::testing::SharedWall {
    let wall = ui::time::testing::SharedWall::default();
    wall.follow();
    wall.pin(ui::clock::capture_wall().expect("the capture instant"));
    wall
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one harness captures the whole parity set in a fixed order"
)]
fn pixel_parity_frames_match_the_old_shells_scenes() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    std::fs::create_dir_all(parity_dir()).expect("create the parity directory");

    // The empty desk, both themes: the old shell's `empty-desk` and
    // `empty-desk` at `--theme light`.
    {
        let mut session = session();
        settle(&mut session, 4);
        capture(&mut session, "empty-desk");
        let mut session = light_session();
        settle(&mut session, 4);
        capture(&mut session, "empty-desk-light");
    }

    // One window: the old shell's `desktop` (one tabbed group).
    {
        let mut session = session();
        let client = mapped(&mut session, "org.example.Terminal");
        settle(&mut session, 4);
        capture(&mut session, "one-window");
        drop(client);
    }

    // Two windows side by side in the strip, and the overview over them.
    {
        let mut session = session();
        let a = mapped(&mut session, "org.example.Terminal");
        let b = mapped(&mut session, "so.tilo.file-explorer");
        settle(&mut session, 6);
        capture(&mut session, "two-windows");
        invoke(&mut session, CHROME_RESOURCE, "toggle-overview");
        settle(&mut session, 8);
        capture(&mut session, "overview");
        drop((a, b));
    }

    // Quick settings, by the bar's own action.
    {
        let (mut session, _host) = session_with_quick_settings();
        settle(&mut session, 2);
        invoke(&mut session, CHROME_RESOURCE, "toggle-quick-settings");
        settle(&mut session, 8);
        capture(&mut session, "quick-settings");
    }

    // Search, opened from the desk.
    {
        use tilo_desktop::desktop_actions::{DESK_RESOURCE, SEARCH_RESOURCE};
        let mut session = session();
        settle(&mut session, 2);
        invoke(&mut session, DESK_RESOURCE, "open-search");
        until_session(&mut session, |session| {
            find_resource(&session.resources(), SEARCH_RESOURCE).is_some()
        });
        settle(&mut session, 8);
        capture(&mut session, "search");
    }

    // One notification banner over the desk, saying what the old shell's
    // `notifications-one` says.
    {
        use tilo_desktop::notifications::{Banner, BannerFeed, Notification, NotificationSeverity};
        let mut session = session();
        settle(&mut session, 2);
        let arrived = ui::time::now()
            .checked_sub(Duration::from_secs(2))
            .unwrap_or_else(ui::time::now);
        session.banners().publish(BannerFeed {
            banners: vec![Banner {
                notification: Notification::new("Build finished", "All 524 tests passed")
                    .with_severity(NotificationSeverity::Info)
                    .with_shown_at(arrived),
                actions: Vec::new(),
            }],
            locked: false,
            held: false,
        });
        settle(&mut session, 12);
        capture(&mut session, "notification");
    }

    // A dialog over a window: the portal asking to share a screen.
    {
        use tilo_sheets::Seat as _;
        let mut session = session();
        let client = mapped(&mut session, "org.example.Terminal");
        settle(&mut session, 4);
        let seat = session.portal().expect("portal sheet").clone();
        seat.presentation().open(false);
        seat.show(Some(Dialog {
            handle: "request-1".to_owned(),
            title: "Share your screen?".to_owned(),
            heading: "Meet".to_owned(),
            detail: "Meet asks to record your screen".to_owned(),
            body: Body::Choices(vec!["Whole screen".to_owned(), "One window".to_owned()]),
            confirm_label: "Allow".to_owned(),
            cancel_label: "Cancel".to_owned(),
            ..Dialog::default()
        }));
        settle(&mut session, 8);
        capture(&mut session, "dialog");
        drop(client);
    }

    // The lock screen over the desk.
    {
        let mut session = Session::headless(
            Options {
                presentation: ui::headless::fixture_runner_inputs,
                user: "ada".to_owned(),
                verifier: std::sync::Arc::new(Sesame),
                collab: false,
                ..Options::default()
            },
            &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        )
        .expect("session");
        let client = mapped(&mut session, "org.example.Terminal");
        settle(&mut session, 2);
        invoke(&mut session, tilo_session::SESSION_RESOURCE, "lock");
        settle(&mut session, 8);
        assert!(session.compositor().locked(), "the session locked");
        capture(&mut session, "lock");
        drop(client);
    }
}

/// Where the desk scenes are written, beside the old shell's captures of the
/// same states.
fn desk_shots_dir() -> std::path::PathBuf {
    std::env::var_os("VISUAL_DESK_OUT").map_or_else(
        || {
            let home = std::env::var("HOME").expect("HOME");
            std::path::PathBuf::from(home).join(".cache/visual-desk/shots/new")
        },
        std::path::PathBuf::from,
    )
}

/// Write the screen as `<dir>/<name>.png`.
fn capture_to(session: &mut Session, dir: &std::path::Path, name: &str) {
    std::fs::create_dir_all(dir).expect("create the shots directory");
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("write the frame");
    // What the frame names, beside it: which state a capture shows, readable
    // without the picture.
    let labels = session.accessible_labels().join("\n");
    std::fs::write(dir.join(format!("{name}.labels")), labels).expect("write the labels");
}

/// Three workspaces on the desk: the first holds a window, the second two,
/// the third none, and the second is in front.
fn desk_of_three(session: &mut Session) -> Vec<TestClient> {
    let a = mapped(session, "org.example.Terminal");
    invoke(session, CHROME_RESOURCE, "new-workspace");
    let b = mapped(session, "so.tilo.file-explorer");
    let c = mapped(session, "org.example.Editor");
    invoke(session, CHROME_RESOURCE, "new-workspace");
    invoke_with(
        session,
        CHROME_RESOURCE,
        "switch-workspace",
        &serde_json::json!({"delta": -1}),
    );
    vec![a, b, c]
}

/// The desk before any window opens, for comparison with the old shell's
/// `empty-desk` scene.
#[test]
fn desk_scene_the_empty_desk() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let dir = desk_shots_dir();
    let mut session = session();
    settle(&mut session, 4);
    capture_to(&mut session, &dir, "desk-empty");
}

/// Three workspaces, two windows in the one in front, for comparison with
/// the old shell's `desktop` and `multi-group` scenes.
#[test]
fn desk_scene_three_workspaces() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let dir = desk_shots_dir();
    let mut session = session();
    let _clients = desk_of_three(&mut session);
    settle(&mut session, 8);
    capture_to(&mut session, &dir, "desk-three");
}

/// The workspace menu over that desk, for comparison with the old shell's
/// `workspace-menu` scene.
#[test]
fn desk_scene_the_workspace_menu() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let dir = desk_shots_dir();
    let mut session = session();
    let _clients = desk_of_three(&mut session);
    settle(&mut session, 8);
    invoke(&mut session, CHROME_RESOURCE, "toggle-workspace-menu");
    settle(&mut session, 8);
    capture_to(&mut session, &dir, "workspace-menu");
}

/// Workspace settings over the desk, for comparison with the old shell's
/// `ws-settings-*` scenes.
#[test]
fn desk_scene_workspace_settings() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let dir = desk_shots_dir();
    let mut session = session();
    let _clients = desk_of_three(&mut session);
    settle(&mut session, 8);
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    settle(&mut session, 8);
    assert!(
        session
            .workspace_settings()
            .expect("the session hosts workspace settings")
            .presentation()
            .is_open(),
        "the settings sheet is open"
    );
    capture_to(&mut session, &dir, "workspace-settings");
}

/// The directory of workspaces over the desk, for comparison with the old
/// shell's `join-empty` and `join-field` scenes.
#[test]
fn desk_scene_the_directory() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let dir = desk_shots_dir();
    let mut session = session();
    let _clients = desk_of_three(&mut session);
    settle(&mut session, 8);
    invoke(&mut session, CHROME_RESOURCE, "join-workspace");
    settle(&mut session, 8);
    assert!(
        session
            .workspace_directory()
            .expect("the session hosts the directory")
            .presentation()
            .is_open(),
        "the directory is open"
    );
    capture_to(&mut session, &dir, "directory");
}

/// The overview open over the three workspaces, for comparison with the old
/// shell's `overview` scene.
#[test]
fn desk_scene_the_overview() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let dir = desk_shots_dir();
    let mut session = session();
    let _clients = desk_of_three(&mut session);
    settle(&mut session, 8);
    let before = session.accessible_labels();
    assert!(
        !directory_open(&session),
        "nothing is open over the desk before the overview is"
    );
    invoke(&mut session, CHROME_RESOURCE, "toggle-overview");
    settle(&mut session, 10);
    assert_ne!(
        session.accessible_labels(),
        before,
        "the overview shows over the desk"
    );
    assert!(
        !directory_open(&session),
        "the overview opens no sheet of its own"
    );
    // The panel takes the display's right-hand 480 px column, inset by the
    // 48 px margin; the desk's windows stand pictured beside it, not under
    // it.
    let panel_column = f64::from(WIDTH) - 480.0 - 48.0;
    let pictures = session.overview_pictures();
    assert!(
        !pictures.is_empty(),
        "the overview pictures the desk's windows"
    );
    for (window, rect) in &pictures {
        assert!(
            rect.0 >= 0.0 && rect.0 + rect.2 <= panel_column,
            "window {window} is pictured clear of the panel's column: {rect:?}"
        );
    }
    capture_to(&mut session, &dir, "overview-three");
}

/// The desk, its bar and the overview on the person's light theme, for
/// comparison with the old shell's `*-light` scenes.
#[test]
fn desk_scene_the_light_theme() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let dir = desk_shots_dir();
    let mut session = light_session();
    settle(&mut session, 6);
    capture_to(&mut session, &dir, "desk-empty-light");
    invoke(&mut session, CHROME_RESOURCE, "toggle-overview");
    settle(&mut session, 10);
    capture_to(&mut session, &dir, "overview-light");
}

/// A session whose presentation is the fixture's, in the light theme.
fn light_runner_inputs() -> ui::application::ComponentRunnerInputs {
    let mut inputs = ui::headless::fixture_runner_inputs();
    inputs.theme = ui::Theme::light();
    inputs
}

/// Frames partway through the overview opening and a workspace switch, at the
/// instants a person sees them as they move.
#[test]
fn desk_motion_frames_match_the_old_shells_films() {
    let _clock = held_clock();
    let dir = desk_shots_dir();
    let mut session = session();
    let _clients = desk_of_three(&mut session);
    settle(&mut session, 6);

    // The overview on its way open, and on its way closed.
    let mut clock = ui::time::now();
    invoke_at(&mut session, clock, "show-overview");
    for (millis, name) in [
        (60_u64, "overview-open-60"),
        (60, "overview-open-120"),
        (80, "overview-open-200"),
    ] {
        capture_at(&mut session, &dir, &mut clock, millis, name);
    }
    capture_at(&mut session, &dir, &mut clock, 500, "overview-open-end");
    assert!(
        frame(&dir, "overview-open-60") != frame(&dir, "overview-open-200"),
        "the overview moves while it opens, rather than appearing at once"
    );
    invoke_at(&mut session, clock, "hide-overview");
    for (millis, name) in [
        (60_u64, "overview-close-60"),
        (60, "overview-close-120"),
        (80, "overview-close-200"),
    ] {
        capture_at(&mut session, &dir, &mut clock, millis, name);
    }
    step_from(&mut session, &mut clock, 500);
    assert!(
        frame(&dir, "overview-close-60") != frame(&dir, "overview-close-120"),
        "the overview moves while it closes"
    );

    // The workspace menu on its way in.
    invoke_at(&mut session, clock, "toggle-workspace-menu");
    for (millis, name) in [
        (60_u64, "workspace-menu-60"),
        (60, "workspace-menu-120"),
        (80, "workspace-menu-200"),
    ] {
        capture_at(&mut session, &dir, &mut clock, millis, name);
    }
    step_from(&mut session, &mut clock, 400);
    invoke_at(&mut session, clock, "toggle-workspace-menu");

    // A workspace switch on its way.
    ui::time::testing::with_monotonic_time(clock, || {
        invoke_with(
            &mut session,
            CHROME_RESOURCE,
            "switch-workspace",
            &serde_json::json!({"delta": -1}),
        );
    });
    for (millis, name) in [
        (60_u64, "workspace-switch-60"),
        (60, "workspace-switch-120"),
        (80, "workspace-switch-200"),
    ] {
        capture_at(&mut session, &dir, &mut clock, millis, name);
    }
    capture_at(&mut session, &dir, &mut clock, 500, "workspace-switch-end");
}

/// The overview shows the windows' own pixels where its cards rest, drawn
/// by the compositor, rather than a mark standing in for them.
#[test]
fn the_overview_pictures_the_desks_windows() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let mut session = session();
    let _client = mapped(&mut session, "org.example.Terminal");
    settle(&mut session, 4);
    assert!(
        session.overview_pictures().is_empty(),
        "nothing is pictured while the overview is away"
    );

    invoke(&mut session, CHROME_RESOURCE, "toggle-overview");
    settle(&mut session, 4);
    let pictures = session.overview_pictures();
    let (window, rect) = pictures
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the window on the desk is pictured: {pictures:?}"));
    assert_eq!(
        Some(window),
        self::window(session.compositor(), "org.example.Terminal").map(|(id, _)| id),
        "the pictured window is the desk's own"
    );
    assert!(
        rect.0 >= 0.0 && rect.1 >= 0.0 && rect.0 + rect.2 <= f64::from(WIDTH),
        "the picture rests on the display: {rect:?}"
    );

    // The compositor draws the window's buffer in the picture: the test
    // client paints itself red.
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    let red = red_pixels(
        &shot.rgba,
        (shot.width, shot.height),
        (rect.0, rect.1, rect.2, rect.3),
    );
    assert!(
        red > 500,
        "the picture shows the window's pixels: {red} red in {rect:?}"
    );
}

/// A window the session keeps private is pictured as the placeholder a
/// capture draws of it: the desk the picture stands on is what a
/// screenshot, a recording and a share of the overview show.
#[test]
fn the_overview_pictures_a_private_window_as_its_placeholder() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let mut session = session();
    let _client = mapped(&mut session, "so.tilo.passwords");
    settle(&mut session, 4);
    invoke(&mut session, CHROME_RESOURCE, "toggle-overview");
    settle(&mut session, 4);
    let pictures = session.overview_pictures();
    let (_, rect) = pictures
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the private window is pictured: {pictures:?}"));

    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    assert_eq!(
        red_pixels(
            &shot.rgba,
            (shot.width, shot.height),
            (rect.0, rect.1, rect.2, rect.3)
        ),
        0,
        "the picture carries none of the window: {rect:?}"
    );

    // A copy of the overview, taken through the capture path, shows the
    // same placeholder.
    let copy = session
        .compositor()
        .read_back_region("HEADLESS-1", (0, 0, WIDTH, HEIGHT))
        .expect("a capture of the output");
    assert_eq!(
        red_pixels(
            &copy.rgba,
            (copy.width, copy.height),
            (rect.0, rect.1, rect.2, rect.3)
        ),
        0,
        "captured, the picture is the placeholder: {rect:?}"
    );
}

/// The person's theme reaches the desk, its bar and the overview, not only
/// the apps.
#[test]
fn the_desk_draws_the_persons_theme() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let regions = |presentation: fn() -> ui::application::ComponentRunnerInputs| {
        let mut session = Session::headless(
            Options {
                presentation,
                collab: false,
                ..Options::default()
            },
            &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        )
        .expect("session");
        settle(&mut session, 4);
        let shot = session
            .compositor()
            .screenshot("HEADLESS-1")
            .expect("screenshot");
        let size = (shot.width, shot.height);
        let desk = (
            mean_luminance(&shot.rgba),
            region_luminance(&shot.rgba, size, (400, 4, 480, 24)),
            region_luminance(&shot.rgba, size, (400, 300, 480, 200)),
        );
        invoke(&mut session, CHROME_RESOURCE, "toggle-overview");
        settle(&mut session, 10);
        let shot = session
            .compositor()
            .screenshot("HEADLESS-1")
            .expect("screenshot");
        let panel = region_luminance(&shot.rgba, (shot.width, shot.height), (800, 300, 400, 220));
        (desk, panel)
    };
    let ((dark, dark_bar, dark_desk), dark_panel) = regions(ui::headless::fixture_runner_inputs);
    let ((light, light_bar, light_desk), light_panel) = regions(light_runner_inputs);
    assert!(
        light > dark + 50.0,
        "the desk follows the person's theme: light {light:.0}, dark {dark:.0}"
    );
    assert!(
        light_bar > 140.0 && dark_bar < 120.0,
        "the bar follows the palette: light {light_bar:.0}, dark {dark_bar:.0}"
    );
    assert!(
        light_desk > 110.0 && dark_desk < 90.0,
        "the desk's ground follows the palette: light {light_desk:.0}, dark {dark_desk:.0}"
    );
    assert!(
        light_panel > 110.0 && dark_panel < 90.0,
        "the overview's panel follows the palette: light {light_panel:.0}, dark {dark_panel:.0}"
    );
}

/// The mean luminance of the `w` by `h` pixels at `(x, y)` of `frame` on a
/// `size` physical frame, 0 to 255.
fn region_luminance(frame: &[u8], size: (u32, u32), rect: (usize, usize, usize, usize)) -> f64 {
    let (width, height) = (size.0 as usize, size.1 as usize);
    let (x0, y0, w, h) = rect;
    let (x1, y1) = ((x0 + w).min(width), (y0 + h).min(height));
    let mut sum = 0.0;
    let mut count = 0.0_f64;
    for y in y0..y1 {
        for x in x0..x1 {
            let at = (y * width + x) * 4;
            sum += 0.2126 * f64::from(frame[at])
                + 0.7152 * f64::from(frame[at + 1])
                + 0.0722 * f64::from(frame[at + 2]);
            count += 1.0;
        }
    }
    sum / count.max(1.0)
}

/// How many pixels of `frame` (`size` physical) inside `rect` (logical) are
/// the red the test client paints itself.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a rect on a drawn frame, clamped to it"
)]
fn red_pixels(frame: &[u8], size: (u32, u32), rect: (f64, f64, f64, f64)) -> u64 {
    let (width, height) = (size.0 as usize, size.1 as usize);
    let scale = f64::from(size.0) / f64::from(u32::try_from(WIDTH).expect("width"));
    let x0 = (rect.0 * scale).max(0.0) as usize;
    let y0 = (rect.1 * scale).max(0.0) as usize;
    let x1 = ((rect.0 + rect.2) * scale).min(f64::from(size.0)) as usize;
    let y1 = ((rect.1 + rect.3) * scale).min(f64::from(size.1)) as usize;
    let mut red = 0;
    for y in y0..y1.min(height) {
        for x in x0..x1.min(width) {
            let at = (y * width + x) * 4;
            let [r, g, b, _] = [frame[at], frame[at + 1], frame[at + 2], frame[at + 3]];
            if r > 170 && g < 90 && b < 90 {
                red += 1;
            }
        }
    }
    red
}

/// The mean luminance of `frame`'s pixels, 0 to 255.
fn mean_luminance(frame: &[u8]) -> f64 {
    let pixels = frame.chunks_exact(4);
    let (sum, count) = pixels.fold((0.0, 0.0_f64), |(sum, count), pixel| {
        let luminance = 0.2126 * f64::from(pixel[0])
            + 0.7152 * f64::from(pixel[1])
            + 0.0722 * f64::from(pixel[2]);
        (sum + luminance, count + 1.0)
    });
    sum / count.max(1.0)
}
