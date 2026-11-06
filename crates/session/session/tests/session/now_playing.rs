//! The song Music plays, on the shell's surfaces: the bar's pill names it,
//! and its card opens from the pill with the song's cover, the point it
//! stands at and what is left of it, and the transport under it. Every state
//! is a test, and every state is written as a picture beside the desk's own.

use super::*;
use tilo_test_env::SpawnUnderEnvLock as _;
use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};

/// The app whose song plays.
const MUSIC: &str = "so.tilo.music";

/// The song's cover, a small `data:` picture as Music sends it: a picture
/// that travels in its own address, read without a content store.
const COVER: &str = ui::headless::FIXTURE_PICTURE;

/// The bar's height, whole logical pixels: everything above stands in the
/// bar, everything below is the desk the cards float over.
const BAR: u32 = 32;

/// The first row under the desk.
fn desk_bottom() -> u32 {
    u32::try_from(HEIGHT).expect("a display with rows")
}

/// Where the captures are written: under the build's `target`, so a build
/// that runs the suite elsewhere hands them back with its artifacts.
fn shots_dir() -> std::path::PathBuf {
    std::env::var_os("VISUAL_ACTIVITY_OUT").map_or_else(
        || {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../target/now-playing-shots")
        },
        std::path::PathBuf::from,
    )
}

/// The song Music publishes while it plays, as its activity carries it: the
/// title, who it is by, the cover, the point the playhead stands at and what
/// is left of it, and the transport beside them.
fn song(artwork: &str) -> Summary {
    song_with(artwork, true)
}

/// The same song as `playing` leaves it: the middle control offers to pause a
/// song that plays and to play one that stands.
fn song_with(artwork: &str, playing: bool) -> Summary {
    Summary::new(Kind::Music, "Glasslight", "")
        .with_id("tk_g1")
        .with_icon("music")
        .with_subtitle("Aurora Skies")
        .with_control(
            Control::new("previous", "Previous", ControlKind::Secondary).with_icon("skip-back"),
        )
        .with_control(
            Control::new(
                "play-pause",
                if playing { "Pause" } else { "Play" },
                ControlKind::Primary,
            )
            .with_icon(if playing { "pause" } else { "play" }),
        )
        .with_control(
            Control::new("next", "Next", ControlKind::Secondary).with_icon("skip-forward"),
        )
        .with_artwork(artwork)
        .with_length(215)
        .with_progress(0.37)
}

/// Show `summary` on the session's board as `app` publishes it: the bar names
/// it, and no card stands over the desk until the person opens it.
fn show(session: &Session, app: &str, summary: Summary) {
    let published = Published::of(summary);
    session.live_activities().show(
        app,
        Some(serde_json::to_value(published).expect("an activity is JSON")),
    );
}

/// The screen as it stands, and its size.
fn shot(session: &mut Session) -> (Vec<u8>, u32) {
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the display is shown");
    (shot.rgba, shot.width)
}

/// How many pixels differ between two frames, in the rows `from..to`.
fn changed(before: &[u8], after: &[u8], width: u32, from: u32, to: u32) -> usize {
    let width = usize::try_from(width).expect("a display with columns");
    let stride = width * 4;
    let mut count = 0;
    for row in usize::try_from(from).expect("a row of the display")
        ..usize::try_from(to).expect("a row of the display")
    {
        let at = row * stride;
        for pixel in 0..width {
            let p = at + pixel * 4;
            if before[p..p + 4] != after[p..p + 4] {
                count += 1;
            }
        }
    }
    count
}

/// Where the song's pill stands on the bar: the middle of what the song
/// painted there, found from `before` and `after` so the pill is pressed
/// wherever its reading puts it, on a wide desk or a phone's.
fn pill_x(before: &[u8], after: &[u8], width: u32) -> f64 {
    let columns = usize::try_from(width).expect("a display with columns");
    let rows = usize::try_from(BAR).expect("rows of the bar");
    let painted = |column: usize| {
        (0..rows).any(|row| {
            let at = (row * columns + column) * 4;
            before[at..at + 4] != after[at..at + 4]
        })
    };
    // The pill is the widest thing the song adds to the bar; anything else
    // that moved beside it (a reading shifted to make room) is narrower.
    let mut widest: Option<(usize, usize)> = None;
    let mut run: Option<(usize, usize)> = None;
    for column in 0..=columns {
        match (column < columns && painted(column), run) {
            (true, Some((first, _))) => run = Some((first, column)),
            (true, None) => run = Some((column, column)),
            (false, Some(span)) => {
                if widest.is_none_or(|widest| span.1 - span.0 > widest.1 - widest.0) {
                    widest = Some(span);
                }
                run = None;
            }
            (false, None) => {}
        }
    }
    let widest = widest.expect("the song painted its pill");
    #[allow(
        clippy::cast_precision_loss,
        reason = "a pill stands inside a display's few thousand columns"
    )]
    let middle = (widest.0 + widest.1) as f64 / 2.0;
    middle
}

/// A left click at `(x, y)`.
fn click(session: &mut Session, x: f64, y: f64) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer moves");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the pointer presses");
    }
}

/// A left click in the middle of the song's pill.
fn click_the_pill(session: &mut Session, before: &[u8], after: &[u8], width: u32) {
    click(session, pill_x(before, after, width), f64::from(BAR / 2));
}

/// The session draws the song's pill in the bar, and the pill opens as the
/// song's card over the desk.
fn pill_and_card(session: &mut Session, dir: &std::path::Path, name: &str, summary: Summary) {
    settle(session, 8);
    let (empty, width) = shot(session);

    show(session, MUSIC, summary);
    settle(session, 8);
    let (named, _) = shot(session);
    assert!(
        changed(&empty, &named, width, 0, BAR) > 100,
        "the bar names the song: {} pixels of it moved",
        changed(&empty, &named, width, 0, BAR)
    );
    assert_eq!(
        changed(&empty, &named, width, BAR, desk_bottom()),
        0,
        "a song never stands on the desk as a card by itself"
    );
    super::desk::capture_to(session, dir, &format!("{name}-pill"));

    click_the_pill(session, &empty, &named, width);
    settle(session, 8);
    let (opened, _) = shot(session);
    assert!(
        changed(&named, &opened, width, BAR, desk_bottom()) > 1_000,
        "the card stands over the desk: {} pixels of it moved",
        changed(&named, &opened, width, BAR, desk_bottom())
    );
    super::desk::capture_to(session, dir, &format!("{name}-card"));
}

/// A session on a display `width` wide, as a phone stands.
fn session_wide(width: i32) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", width, HEIGHT)],
    )
    .expect("session")
}

/// The same, drawn in the light theme.
fn light_session_wide(width: i32) -> Session {
    Session::headless(
        Options {
            presentation: super::desk::light_presentation,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", width, HEIGHT)],
    )
    .expect("session")
}

/// The width a phone's display stands at.
const PHONE: i32 = 420;

/// The song's pill and card on the dark desk.
#[test]
fn the_song_stands_in_the_bar_and_opens_as_its_card() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = session();
    pill_and_card(&mut session, &dir, "u2-dark", song(COVER));
}

/// The same song with its player paused: the middle control offers to play
/// it, so the card is not the one a playing song draws.
#[test]
fn a_paused_song_offers_to_play_again() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut paused = session();
    pill_and_card(&mut paused, &dir, "u2-dark-paused", song_with(COVER, false));
    let (paused_card, width) = shot(&mut paused);

    let mut playing = session();
    pill_and_card(&mut playing, &dir, "u2-dark-playing", song(COVER));
    let (playing_card, _) = shot(&mut playing);

    let differs = changed(&playing_card, &paused_card, width, BAR, desk_bottom());
    assert!(
        differs > 20,
        "a paused song's card offers another control than a playing one's: {differs} pixels"
    );
}

/// A song whose player sends no cover: the card draws the song's glyph in the
/// artwork's place, so the row keeps its shape.
#[test]
fn a_song_without_a_cover_keeps_the_card_its_shape() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = session();
    pill_and_card(&mut session, &dir, "u2-dark-no-art", song(""));
}

/// A phone's desk keeps the song in the pill, and expanding it drops the card
/// full-width from the bar.
#[test]
fn a_phone_names_the_song_in_the_pill_and_drops_its_card_from_the_bar() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = session_wide(PHONE);
    pill_and_card(&mut session, &dir, "u2-phone", song(COVER));
}

/// The same on a phone drawing the light theme.
#[test]
fn a_phone_in_the_light_theme_drops_the_song_card_from_the_bar() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = light_session_wide(PHONE);
    pill_and_card(&mut session, &dir, "u2-phone-light", song(COVER));
}

/// The song's pill and card on the light desk.
#[test]
fn the_song_stands_in_the_light_bar_and_opens_as_its_card() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = super::desk::light_session();
    pill_and_card(&mut session, &dir, "u2-light", song(COVER));
}

/// The card paints the song's cover: the same card without one draws its
/// place differently.
#[test]
fn the_card_paints_the_songs_cover() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let mut bare_session = session();
    settle(&mut bare_session, 8);
    let (empty, width) = shot(&mut bare_session);

    show(&bare_session, MUSIC, song(""));
    settle(&mut bare_session, 8);
    let (named, _) = shot(&mut bare_session);
    click_the_pill(&mut bare_session, &empty, &named, width);
    settle(&mut bare_session, 8);
    let (bare, _) = shot(&mut bare_session);

    let mut covered_session = session();
    settle(&mut covered_session, 8);
    let (empty, _) = shot(&mut covered_session);
    show(&covered_session, MUSIC, song(COVER));
    settle(&mut covered_session, 8);
    let (named, _) = shot(&mut covered_session);
    click_the_pill(&mut covered_session, &empty, &named, width);
    settle(&mut covered_session, 8);
    let (covered, _) = shot(&mut covered_session);

    let painted = changed(&bare, &covered, width, BAR, desk_bottom());
    assert!(
        painted > 500,
        "the cover fills the card's artwork tile: {painted} pixels of it moved"
    );
}

/// The cover the player publishes, as the audio service reads one: its size
/// and its pixels.
fn cover() -> serde_json::Value {
    serde_json::json!({"width": 16, "height": 16, "rgba": "Oi6W/0Ezkf9JOIz/UD2I/1hCg/9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/0Ezkf9JOIz/UD2I/1hCg/9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/9JOIz/UD2I/1hCg/9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/UD2I/1hCg/9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/1hCg/9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL/92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf/9sBz/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf/9sBz//7UY/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf/9sBz//7UY//+6E/+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf/9sBz//7UY//+6E///vw7/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf/9sBz//7UY//+6E///vw7//8QK/w=="})
}

/// The song a player publishes while it plays, as the audio service publishes
/// it: its title, who it is by, its cover, where the playhead stands and how
/// long it is.
pub(super) fn playing_song() -> serde_json::Value {
    serde_json::json!({
        "players": [{
            "token": "music",
            "bus": "org.mpris.MediaPlayer2.music",
            "identity": "Music",
            "title": "Glasslight",
            "artist": "Aurora Skies",
            "playing": true,
            "paused": false,
            "can-play": true,
            "can-pause": true,
            "can-previous": true,
            "can-next": true,
            "can-seek": true,
            "position-ms": 79_500,
            "duration-ms": 215_000,
            "art": cover(),
        }],
        "outputs": [{"token": "local", "label": "On this computer", "current": true}],
    })
}

/// The same player standing paused.
fn paused_song() -> serde_json::Value {
    let mut song = playing_song();
    song["players"][0]["playing"] = serde_json::json!(false);
    song["players"][0]["paused"] = serde_json::json!(true);
    song
}

/// The same player, its cover not yet loaded.
fn song_without_a_cover() -> serde_json::Value {
    let mut song = playing_song();
    song["players"][0]["art"] = serde_json::Value::Null;
    song
}

/// A session whose quick settings panel is the app this repository builds,
/// and whose player publishes `published`'s reading of what plays.
fn panel_session(published: &super::lock::Publishing) -> (Session, tempfile::TempDir) {
    let home = super::lock::private_home();
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let panel = quick_settings::surface_of(tilo_test_env::sibling_binary(
        "tilo-quick-settings",
        "tilo-quick-settings",
    ));
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: vec![Box::new(super::lock::PlayerService::new(
                asked,
                published.clone(),
            ))],
            action_plane: tilo_session::ActionPlane::At(home.path().join("collab.sock")),
            surfaces: vec![panel],
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    (session, home)
}

/// The panel draws the song that plays: the Quick Settings card names it with
/// its cover, where it stands and the transport under it.
fn panel_names_the_song(name: &str, playing: serde_json::Value) {
    let dir = shots_dir();
    let published = super::lock::Publishing::default();
    let (mut session, _home) = panel_session(&published);
    settle(&mut session, 4);
    let (closed, width) = shot(&mut session);

    // The panel opens with nothing playing: it stands over the desk with no
    // card in it.
    invoke(&mut session, CHROME_RESOURCE, "quick-settings");
    let silent = crate::common::wait_for(&mut session, "the panel opens", |session| {
        let drawn = session
            .compositor()
            .screenshot("HEADLESS-1")
            .expect("the display is shown");
        (changed(&closed, &drawn.rgba, width, 0, desk_bottom()) > 10_000).then_some(drawn)
    });

    // A song starts playing: the panel's card names it, where it stands and
    // what the player is asked for.
    published.set_playing(Some(playing));
    let drawn = crate::common::wait_for(&mut session, "the panel draws the song", |session| {
        let drawn = session
            .compositor()
            .screenshot("HEADLESS-1")
            .expect("the display is shown");
        (changed(&silent.rgba, &drawn.rgba, width, 0, desk_bottom()) > 1_000).then_some(drawn)
    });
    assert!(
        changed(&silent.rgba, &drawn.rgba, width, 0, desk_bottom()) > 1_000,
        "the panel names the song that plays"
    );
    super::desk::capture_to(&mut session, &dir, name);
}

/// The song Quick Settings names, on the dark desk.
#[test]
fn quick_settings_names_the_song_that_plays() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    panel_names_the_song("u2-quick-settings", playing_song());
}

/// The song the panel names while it stands paused, and while the player has
/// not sent its cover: the middle control offers to play, and the artwork
/// stands as the song's glyph.
#[test]
fn quick_settings_names_a_paused_song_and_one_without_a_cover() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    panel_names_the_song("u2-quick-settings-paused", paused_song());
    panel_names_the_song("u2-quick-settings-no-art", song_without_a_cover());
}

/// One capture the app at `app` draws of `scenario` in `theme`, as the frame
/// it wrote: a surface app draws in the theme the person's settings give it,
/// which in a test session is not the theme the session's own parts take, so
/// the app's capture frontend draws the same screen in the theme named.
fn app_capture(
    app: &std::path::Path,
    beside: &std::path::Path,
    scenario: &str,
    theme: &str,
) -> image::RgbaImage {
    let at = beside.join(format!("{scenario}.png"));
    let drawn = std::process::Command::new(app)
        .args(["--scenario", scenario, "--shots"])
        .arg(&at)
        .env("TILO_SHOTS_THEME", theme)
        .output_locked()
        .expect("the app draws its capture");
    assert!(
        drawn.status.success(),
        "{} draws {scenario}: {}",
        app.display(),
        String::from_utf8_lossy(&drawn.stderr)
    );
    image::open(&at)
        .expect("the capture is a picture")
        .to_rgba8()
}

/// How bright a frame is, of 255: the light theme draws the screen light, the
/// dark one dark.
fn brightness(frame: &[u8]) -> f64 {
    let (sum, count) = frame
        .chunks_exact(4)
        .fold((0.0, 0.0), |(sum, count), pixel| {
            let grey = (f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2])) / 3.0;
            (sum + grey, count + 1.0)
        });
    sum / count
}

/// The song Quick Settings names, in the light theme: its card over the
/// panel's page, beside the same panel with nothing playing.
#[test]
fn quick_settings_names_the_song_in_the_light_theme() {
    let _settled = ui::motion::testing::settled();
    let dir = shots_dir();
    std::fs::create_dir_all(&dir).expect("create the shots directory");
    let panel = tilo_test_env::sibling_binary("tilo-quick-settings", "tilo-quick-settings");
    let beside = tempfile::Builder::new()
        .prefix("panel-light-")
        .tempdir_in(&dir)
        .expect("a directory beside the captures");
    let quiet = app_capture(&panel, beside.path(), "controls", "light");
    let sung = app_capture(&panel, beside.path(), "now-playing", "light");
    let (width, height) = (sung.width(), sung.height());
    let drawn = changed(quiet.as_raw(), sung.as_raw(), width, 0, height);
    assert!(
        drawn > 500,
        "the panel shows the song's card over the page it shows without one: {drawn} pixels"
    );
    assert!(
        brightness(sung.as_raw()) > 150.0,
        "the panel is drawn in the light theme: {} of 255",
        brightness(sung.as_raw())
    );
    std::fs::copy(beside.path().join("now-playing.png"), dir.join("u2-quick-settings-light.png"))
        .expect("the capture is written");
}

/// The lock screen's card shows the song that plays, its cover and where the
/// playhead stands.
fn lock_card(name: &str, song: serde_json::Value, media: fn(&serde_json::Value) -> bool) {
    let dir = shots_dir();
    let mut locked = super::lock::Locked::new();
    let quiet = locked.quiet.clone();
    let playing = locked.plays(Some(song), media, &quiet);
    std::fs::create_dir_all(&dir).expect("create the shots directory");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &playing.rgba,
        playing.width,
        playing.height,
        image::ColorType::Rgba8,
    )
    .expect("write the frame");
}

/// The song the lock screen shows, on the desk as the session draws it.
#[test]
fn the_lock_screen_shows_the_song_that_plays() {
    let _settled = ui::motion::testing::settled();
    lock_card("u2-lock", playing_song(), |media| {
        media["title"] == "Glasslight" && media["playing"] == true
    });
}

/// The same song standing paused: the middle control offers to play it.
#[test]
fn the_lock_screen_shows_a_paused_song() {
    let _settled = ui::motion::testing::settled();
    lock_card("u2-lock-paused", paused_song(), |media| {
        media["title"] == "Glasslight" && media["playing"] == false
    });
}

/// The same song whose player has not sent its cover: the artwork stands as
/// the song's glyph.
#[test]
fn the_lock_screen_shows_a_song_without_a_cover() {
    let _settled = ui::motion::testing::settled();
    lock_card("u2-lock-no-art", song_without_a_cover(), |media| {
        media["title"] == "Glasslight" && media["artwork"] == ""
    });
}

/// The song the lock screen shows, in the light theme: its card beside the
/// same screen with nothing playing.
#[test]
fn the_lock_screen_shows_the_song_in_the_light_theme() {
    let _settled = ui::motion::testing::settled();
    let dir = shots_dir();
    std::fs::create_dir_all(&dir).expect("create the shots directory");
    let beside = tempfile::Builder::new()
        .prefix("lock-light-")
        .tempdir_in(&dir)
        .expect("a directory beside the captures");
    let app = super::lock::lock_screen().program;
    let quiet = app_capture(&app, beside.path(), "plain", "light");
    let drawn = app_capture(&app, beside.path(), "playing", "light");
    let (width, height) = (drawn.width(), drawn.height());
    let painted = changed(quiet.as_raw(), drawn.as_raw(), width, 0, height);
    assert!(
        painted > 500,
        "the lock screen draws the song's card over the screen it shows without one: {painted} pixels"
    );
    assert!(
        brightness(drawn.as_raw()) > 150.0,
        "the screen is drawn in the light theme: {} of 255",
        brightness(drawn.as_raw())
    );
    std::fs::copy(beside.path().join("playing.png"), dir.join("u2-lock-light.png"))
        .expect("the capture is written");
}
