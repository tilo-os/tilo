//! The shell's pictures: what the frame loop draws of the shell, its bar, its panels and the
//! transitions between them. Each state is a test of its own with its own session, so what one
//! leaves open never shows in another's picture. A test that keeps a frame for a person to look at
//! writes it to `$PIXEL_PARITY_OUT` (default `~/.cache/pixel-parity/new`).

use super::*;

/// Where a frame a test keeps is written.
fn captures_dir() -> std::path::PathBuf {
    std::env::var_os("PIXEL_PARITY_OUT").map_or_else(
        || {
            let home = std::env::var("HOME").expect("HOME");
            std::path::PathBuf::from(home).join(".cache/pixel-parity/new")
        },
        std::path::PathBuf::from,
    )
}

/// Write the screen as `name.png` into [`captures_dir`].
pub(super) fn capture(session: &mut Session, name: &str) {
    let dir = captures_dir();
    std::fs::create_dir_all(&dir).expect("create the captures' directory");
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

/// Run the session's frame loop on `millis` further than `clock` stands, at the pace a display
/// presents frames, with the clock pinned to each frame's instant: a motion then draws where it
/// stands that far in, the same on every run whatever a frame costs to draw.
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

/// Perform the chrome action with `arguments` at `clock` 's instant, so what it starts moves from
/// there.
fn invoke_with_at(
    session: &mut Session,
    clock: std::time::Instant,
    action: &str,
    arguments: &serde_json::Value,
) {
    ui::time::testing::with_monotonic_time(clock, || {
        invoke_with(session, CHROME_RESOURCE, action, arguments);
    });
}

/// A session drawing the shell in the light theme.
pub(super) fn light_session() -> Session {
    Session::headless(
        Options {
            presentation: light_presentation,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// The presentation of a session drawing the shell in the light theme.
pub(super) fn light_presentation() -> ui::application::ComponentRunnerInputs {
    let mut inputs = ui::headless::fixture_runner_inputs();
    inputs.theme = ui::theme::Theme::light();
    inputs
}

/// Hold this test's wall clock at the capture instant, as the launcher holds a capture's: the bar's
/// date and time are then the same on every run, and a frame in which nothing but the real minute
/// turned over is not a change. Only this thread follows it, so the other tests of the process read
/// their own clock.
pub(super) fn held_clock() -> ui::time::testing::SharedWall {
    tilo_test_env::held_wall_clock()
}

/// The readout names the device it changed, and the level is the bar: a pill as wide as what it
/// reads, two rows (the device and the value above the bar) for a level, one for a mode change.
#[test]
fn the_readout_names_its_device_and_the_level_is_the_bar() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let mut played = session();
    settle(&mut played, 4);
    let before = played
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");

    invoke_with(
        &mut played,
        tilo_session_core::desktop::osd::OSD_RESOURCE,
        "show",
        &serde_json::json!({
            "kind": "volume",
            "level": 0.65,
            "device": "Studio headphones",
            "device-glyph": "headphones",
        }),
    );
    settle(&mut played, 2);
    let after = played
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    assert!(
        played
            .accessible_labels()
            .iter()
            .any(|label| label.contains("Studio headphones, 65%")),
        "the readout says whose level it is to a screen reader: {:?}",
        played.accessible_labels()
    );

    // On a 1280 by 800 output the pill rests one step of sixty-four above the screen's foot,
    // centred, two rows of a device name and value above the bar: nothing of the shell lights up
    // outside it, so it takes no more room than what it reads.
    let (left, top, right, bottom) =
        lit_bounds(&before.rgba, &after.rgba, (after.width, after.height))
            .expect("the readout paints something");
    let (width, centre) = (right - left, usize::midpoint(left, right));
    assert!((668..=676).contains(&top), "the pill's top is at {top}");
    assert!(
        (733..=739).contains(&bottom),
        "the pill's bottom is at {bottom}"
    );
    assert!(
        (60..=68).contains(&(bottom - top)),
        "the pill is {} high",
        bottom - top
    );
    assert!(
        (636..=644).contains(&centre),
        "the pill is centred at {centre}"
    );
    assert!(
        (150..=320).contains(&width),
        "the pill is {width} wide: as wide as what it reads"
    );

    // A mode change is one row of words, so its pill is shorter than a level's and rests on the
    // same line.
    let mut changed = session();
    settle(&mut changed, 4);
    let before = changed
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    invoke_with(
        &mut changed,
        tilo_session_core::desktop::osd::OSD_RESOURCE,
        "show",
        &serde_json::json!({"kind": "night-light", "on": true}),
    );
    settle(&mut changed, 2);
    let after = changed
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    assert!(
        changed
            .accessible_labels()
            .iter()
            .any(|label| label == "Night light on"),
        "a mode change is said in words: {:?}",
        changed.accessible_labels()
    );
    let (_, top, _, bottom) = lit_bounds(&before.rgba, &after.rgba, (after.width, after.height))
        .expect("the hint paints something");
    assert!(
        (44..=52).contains(&(bottom - top)),
        "the hint pill is {} high: one row",
        bottom - top
    );
    assert!(
        (733..=739).contains(&bottom),
        "the hint's bottom is at {bottom}"
    );
}

/// The bounds `(left, top, right, bottom)` in logical pixels of everything `frame` (`size`
/// physical) paints brighter than `before` : the lit body of a floating surface, without the shadow
/// it also darkens around itself.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "a drawn frame's pixels, read back at its size"
)]
fn lit_bounds(
    before: &[u8],
    frame: &[u8],
    size: (u32, u32),
) -> Option<(usize, usize, usize, usize)> {
    let (width, height) = (size.0 as usize, size.1 as usize);
    let scale = f64::from(size.0) / f64::from(u32::try_from(WIDTH).expect("width"));
    let mut bounds: Option<(usize, usize, usize, usize)> = None;
    for y in 0..height {
        for x in 0..width {
            let at = (y * width + x) * 4;
            if luma(&frame[at..at + 4]) <= luma(&before[at..at + 4]) + 2 {
                continue;
            }
            let (lx, ly) = ((x as f64 / scale) as usize, (y as f64 / scale) as usize);
            bounds = Some(match bounds {
                None => (lx, ly, lx + 1, ly + 1),
                Some((left, top, right, bottom)) => (
                    left.min(lx),
                    top.min(ly),
                    right.max(lx + 1),
                    bottom.max(ly + 1),
                ),
            });
        }
    }
    bounds
}

/// How bright a pixel is, summed over its channels.
fn luma(pixel: &[u8]) -> u16 {
    u16::from(pixel[0]) + u16::from(pixel[1]) + u16::from(pixel[2])
}

/// Write the screen as `<dir>/<name>.png` .
pub(super) fn capture_to(session: &mut Session, dir: &std::path::Path, name: &str) {
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
    // What the frame names, beside it: which state a capture shows, readable without the picture.
    let labels = session.accessible_labels().join("\n");
    std::fs::write(dir.join(format!("{name}.labels")), labels).expect("write the labels");
}

/// A session whose presentation is the fixture's, in the light theme.
fn light_runner_inputs() -> ui::application::ComponentRunnerInputs {
    let mut inputs = ui::headless::fixture_runner_inputs();
    inputs.theme = ui::Theme::light();
    inputs
}

/// The screen `millis` after `clock` stood, with the frame loop run until then.
fn screen_at(session: &mut Session, clock: &mut std::time::Instant, millis: u64) -> Vec<u8> {
    step_from(session, clock, millis);
    session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot")
        .rgba
}

/// The overview is carried by its motion: it is on its way in the frames a person sees while it
/// opens, and on its way out in the ones while it closes, rather than appearing and vanishing at
/// once.
#[test]
fn the_overview_moves_while_it_opens_and_closes() {
    let _clock = held_clock();
    let mut session = session();
    let _clients = shell_of_three_workspaces(&mut session);
    settle(&mut session, 6);

    let mut clock = ui::time::now();
    invoke_with_at(
        &mut session,
        clock,
        "overview",
        &serde_json::json!({"open": true}),
    );
    let early = screen_at(&mut session, &mut clock, 60);
    let later = screen_at(&mut session, &mut clock, 140);
    assert_ne!(
        early, later,
        "the overview moves while it opens, rather than appearing at once"
    );
    step_from(&mut session, &mut clock, 500);

    invoke_with_at(
        &mut session,
        clock,
        "overview",
        &serde_json::json!({"open": false}),
    );
    let closing = screen_at(&mut session, &mut clock, 60);
    let closed_later = screen_at(&mut session, &mut clock, 60);
    assert_ne!(
        closing, closed_later,
        "the overview moves while it closes, rather than vanishing at once"
    );
}

/// Three workspaces on the shell: the first holds a window, the second two, and the second is in
/// front.
fn shell_of_three_workspaces(session: &mut Session) -> Vec<TestClient> {
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

/// The overview shows the windows' own pixels where its cards rest, drawn by the compositor, rather
/// than a mark in place of them.
#[test]
fn the_overview_pictures_the_shells_windows() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let mut session = session();
    let _client = mapped(&mut session, "org.example.Terminal");
    settle(&mut session, 4);
    assert!(
        session.overview_pictures().is_empty(),
        "nothing is pictured while the overview is away"
    );

    invoke(&mut session, CHROME_RESOURCE, "overview");
    settle(&mut session, 4);
    assert!(
        !session
            .workspace_directory()
            .is_some_and(|directory| directory.presentation().is_open()),
        "the overview opens no sheet of its own"
    );
    let pictures = session.overview_pictures();
    let (window, rect) = pictures
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the window on the shell is pictured: {pictures:?}"));
    assert_eq!(
        Some(window),
        self::window(session.compositor(), "org.example.Terminal").map(|(id, _)| id),
        "the pictured window is the shell's own"
    );
    assert!(
        rect.0 >= 0.0 && rect.1 >= 0.0 && rect.0 + rect.2 <= f64::from(WIDTH),
        "the picture rests on the display: {rect:?}"
    );
    // The overview takes the display's right-hand 480 px column, inset by the 48 px margin: the
    // shell's windows stand pictured beside it, not under it.
    let panel_column = f64::from(WIDTH) - 480.0 - 48.0;
    for (window, rect) in &pictures {
        assert!(
            rect.0 >= 0.0 && rect.0 + rect.2 <= panel_column,
            "window {window} is pictured clear of the panel's column: {rect:?}"
        );
    }

    // The compositor draws the window's buffer in the picture: the test client paints itself red.
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

/// Every picture in the overview carries pixels: the card stands where the window's own buffer is
/// drawn, so a window that showed text on its strip shows it there too. A card that stays the bare
/// ground is a window the person cannot recognise.
#[test]
fn every_overview_card_shows_the_window_it_pictures() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let mut session = session();
    // A client whose buffer differs from the card's ground: a dark terminal, drawn green.
    let mut client = mapped(&mut session, "org.example.Terminal");
    let (width, height) = client
        .seen
        .size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((300, 200));
    client.show(width, height, [30, 200, 60, 255]);
    // The compositor takes the frame the client just committed.
    pump(&mut session, &mut [&mut client], |session, _| {
        let shot = session
            .compositor()
            .screenshot("HEADLESS-1")
            .expect("a frame");
        green_pixels(
            &shot.rgba,
            (shot.width, shot.height),
            (0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)),
        ) > 500
    });
    settle(&mut session, 6);

    // The window stands on a strip of its own, and the person works in the first strip: the
    // overview pictures every strip of the workspace, not only the one on screen, so the window on
    // the strip off screen still shows its own pixels.
    let tab = tab_of(&session, "org.example.Terminal");
    add_strip(&session, "Second");
    let second = strip_id(&session, "Second");
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "move-tab",
        &serde_json::json!({"tab": tab.to_string(), "to": {"strip": second}}),
    );
    // Back to the first strip: the window is now on a strip the person is not looking at.
    invoke_with(
        &mut session,
        CHROME_RESOURCE,
        "step-strip",
        &serde_json::json!({"steps": -1}),
    );
    for _ in 0..20 {
        client.exchange();
        session.step(Duration::from_millis(4)).expect("step");
    }
    // The window is on the second strip, and the person is looking at the first: nothing of it is
    // on screen, so a card that shows its pixels shows them because the overview pictures every
    // strip, not because the window happens to be in front.
    let shown = session
        .handle()
        .read(tilo_desktop_model::Layout::active_strip_id)
        .expect("a strip is shown");
    let holding = session.handle().read(|tree| {
        tree.active_workspace()
            .and_then(|workspace| {
                workspace.strips.iter().find(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .any(|group| group.tabs.iter().any(|held| held.id == tab))
                })
            })
            .map(|strip| strip.id)
    });
    assert!(
        holding.is_some_and(|strip| strip != shown),
        "the window stands on a strip the person is not looking at: {holding:?} against {shown:?}"
    );

    invoke(&mut session, CHROME_RESOURCE, "overview");
    settle(&mut session, 6);
    let pictures = session.overview_pictures();
    let (_, rect) = pictures
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the window on its own strip is pictured: {pictures:?}"));
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    let green = green_pixels(&shot.rgba, (shot.width, shot.height), rect);
    assert!(
        green > 500,
        "the card shows the window's pixels, not its ground: {green} green in {rect:?}"
    );
}

/// A card fills with its window: the picture is drawn at the window's own aspect, filling the
/// card's width, never shrunk into a corner of it with the ground showing round it.
#[test]
fn an_overview_card_fills_at_the_windows_aspect() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let mut session = session();
    let mut client = mapped(&mut session, "org.example.Terminal");
    // Draw the window in a colour no card's ground is, at the shape the desktop gave it.
    let (width, height) = client
        .seen
        .size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((300, 200));
    client.show(width, height, [30, 200, 60, 255]);
    pump(&mut session, &mut [&mut client], |session, _| {
        let shot = session
            .compositor()
            .screenshot("HEADLESS-1")
            .expect("a frame");
        green_pixels(
            &shot.rgba,
            (shot.width, shot.height),
            (0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)),
        ) > 500
    });
    settle(&mut session, 6);

    invoke(&mut session, CHROME_RESOURCE, "overview");
    settle(&mut session, 6);
    let pictures = session.overview_pictures();
    let (_, rect) = pictures
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the window on the shell is pictured: {pictures:?}"));
    let (x, y, w, h) = rect;
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    let (size, frame) = ((shot.width, shot.height), shot.rgba.as_slice());
    // The picture's own bounds, where its pixels are, inside the card it rests in.
    let bounds = green_bounds(frame, size, rect)
        .unwrap_or_else(|| panic!("the card's picture has pixels: {rect:?}"));
    let (dx, dy, dw, dh) = bounds;
    // The window is the shape of its buffer, and the picture keeps that shape: a picture that
    // shrank into a corner leaves bars of the card's ground at its right and its bottom.
    let window = f64::from(width) / f64::from(height);
    let drawn = dw / dh;
    assert!(
        (drawn - window).abs() < 0.02 * window,
        "the picture keeps the window's shape: {drawn} against {window}"
    );
    assert!(
        (dx - x).abs() <= 1.0
            && (dy - y).abs() <= 1.0
            && (dw - w).abs() <= 1.0
            && (dh - h).abs() <= 1.0,
        "the picture fills its card: {:?} in {rect:?}",
        (dx, dy, dw, dh)
    );
}

/// A window the session keeps private is pictured as the placeholder a capture draws of it: the
/// shell the picture stands on is what a screenshot, a recording and a share of the overview show.
#[test]
fn the_overview_pictures_a_private_window_as_its_placeholder() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let mut session = session();
    let _client = mapped(&mut session, "so.tilo.passwords");
    settle(&mut session, 4);
    invoke(&mut session, CHROME_RESOURCE, "overview");
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

    // A copy of the overview, taken through the capture path, shows the same placeholder.
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

/// The person's theme reaches the shell, its bar and the overview, not only the apps.
#[test]
fn the_shell_draws_the_persons_theme() {
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
        let shell = (
            mean_luminance(&shot.rgba),
            region_luminance(&shot.rgba, size, (400, 4, 480, 24)),
            region_luminance(&shot.rgba, size, (400, 300, 480, 200)),
        );
        invoke(&mut session, CHROME_RESOURCE, "overview");
        settle(&mut session, 10);
        let shot = session
            .compositor()
            .screenshot("HEADLESS-1")
            .expect("screenshot");
        let panel = region_luminance(&shot.rgba, (shot.width, shot.height), (800, 300, 400, 220));
        (shell, panel)
    };
    let ((dark, dark_bar, dark_shell), dark_panel) = regions(ui::headless::fixture_runner_inputs);
    let ((light, light_bar, light_shell), light_panel) = regions(light_runner_inputs);
    assert!(
        light > dark + 50.0,
        "the shell follows the person's theme: light {light:.0}, dark {dark:.0}"
    );
    assert!(
        light_bar > 140.0 && dark_bar < 120.0,
        "the bar follows the palette: light {light_bar:.0}, dark {dark_bar:.0}"
    );
    assert!(
        light_shell > 110.0 && dark_shell < 90.0,
        "the shell's ground follows the palette: light {light_shell:.0}, dark {dark_shell:.0}"
    );
    assert!(
        light_panel > 110.0 && dark_panel < 90.0,
        "the overview's panel follows the palette: light {light_panel:.0}, dark {dark_panel:.0}"
    );
}

/// The mean luminance of the `w` by `h` pixels at `(x, y)` of `frame` on a `size` physical frame, 0
/// to 255.
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

/// How many pixels of `frame` (`size` physical) inside `rect` (logical) are the red the test client
/// paints itself.
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

/// How many pixels of `frame` (`size` physical) inside `rect` (logical) are the green the test
/// client paints itself: a window whose pixels are not the card's own ground.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a rect on a drawn frame, clamped to it"
)]
fn green_pixels(frame: &[u8], size: (u32, u32), rect: (f64, f64, f64, f64)) -> u64 {
    let (width, height) = (size.0 as usize, size.1 as usize);
    let scale = f64::from(size.0) / f64::from(u32::try_from(WIDTH).expect("width"));
    let x0 = (rect.0 * scale).max(0.0) as usize;
    let y0 = (rect.1 * scale).max(0.0) as usize;
    let x1 = ((rect.0 + rect.2) * scale).min(f64::from(size.0)) as usize;
    let y1 = ((rect.1 + rect.3) * scale).min(f64::from(size.1)) as usize;
    let mut green = 0;
    for y in y0..y1.min(height) {
        for x in x0..x1.min(width) {
            let at = (y * width + x) * 4;
            let [r, g, b, _] = [frame[at], frame[at + 1], frame[at + 2], frame[at + 3]];
            if g > 150 && r < 90 && b < 110 {
                green += 1;
            }
        }
    }
    green
}

/// Where the test client's green pixels lie inside `rect` (logical), as `(x, y, width, height)`
/// in the frame's logical coordinates: `None` when none is there.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a rect on a drawn frame, clamped to it"
)]
fn green_bounds(
    frame: &[u8],
    size: (u32, u32),
    rect: (f64, f64, f64, f64),
) -> Option<(f64, f64, f64, f64)> {
    let (width, height) = (size.0 as usize, size.1 as usize);
    let scale = f64::from(size.0) / f64::from(u32::try_from(WIDTH).expect("width"));
    let x0 = (rect.0 * scale).max(0.0) as usize;
    let y0 = (rect.1 * scale).max(0.0) as usize;
    let x1 = ((rect.0 + rect.2) * scale).min(f64::from(size.0)) as usize;
    let y1 = ((rect.1 + rect.3) * scale).min(f64::from(size.1)) as usize;
    let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
    for y in y0..y1.min(height) {
        for x in x0..x1.min(width) {
            let at = (y * width + x) * 4;
            let [r, g, b, _] = [frame[at], frame[at + 1], frame[at + 2], frame[at + 3]];
            if g > 150 && r < 90 && b < 110 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x);
                bottom = bottom.max(y);
            }
        }
    }
    (right > left).then(|| {
        (
            left as f64 / scale,
            top as f64 / scale,
            (right - left + 1) as f64 / scale,
            (bottom - top + 1) as f64 / scale,
        )
    })
}

/// The mean luminance of `frame` 's pixels, 0 to 255.
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
