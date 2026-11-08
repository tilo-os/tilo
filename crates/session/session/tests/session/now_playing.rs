//! The song Music plays, on the shell's surfaces: the bar's pill names it,
//! and its card opens from the pill with the song's cover, the point it
//! stands at and what is left of it, and the transport under it. Every state
//! is a test, and every state is written as a picture beside the desk's own.

use super::*;
use tilo_test_env::SpawnUnderEnvLock as _;
use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};

/// The app whose song plays.
const MUSIC: &str = "so.tilo.music";

/// The song's cover, a small `data:` picture as Music sends it: a picture that travels in its own
/// address, read without a content store.
const COVER: &str = ui::headless::FIXTURE_PICTURE;

/// The bar's height, whole logical pixels: everything above stands in the bar, everything below is
/// the desk the cards float over.
const BAR: u32 = 32;

/// The first row under the desk.
fn desk_bottom() -> u32 {
    u32::try_from(HEIGHT).expect("a display with rows")
}

/// Where the captures are written: under the build's `target` , so a build that runs the suite
/// elsewhere hands them back with its artifacts.
fn shots_dir() -> std::path::PathBuf {
    std::env::var_os("VISUAL_ACTIVITY_OUT").map_or_else(
        || {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../target/now-playing-shots")
        },
        std::path::PathBuf::from,
    )
}

/// The song Music publishes while it plays, as its activity carries it: the title, who it is by,
/// the cover, the point the playhead stands at and what is left of it, and the transport beside
/// them.
fn song(artwork: &str) -> Summary {
    song_with(artwork, true)
}

/// The same song as `playing` leaves it: the middle control offers to pause a song that plays and
/// to play one that stands.
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

/// Show `summary` on the session's board as `app` publishes it: the bar names it, and no card
/// stands over the desk until the person opens it.
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

/// How many pixels differ between two frames, in the rows `from..to` .
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

/// Where the song's pill stands on the bar: the middle of what the song painted there, found from
/// `before` and `after` so the pill is pressed wherever its reading puts it, on a wide desk or a
/// phone's.
fn pill_x(before: &[u8], after: &[u8], width: u32) -> f64 {
    let columns = usize::try_from(width).expect("a display with columns");
    let rows = usize::try_from(BAR).expect("rows of the bar");
    let painted = |column: usize| {
        (0..rows).any(|row| {
            let at = (row * columns + column) * 4;
            before[at..at + 4] != after[at..at + 4]
        })
    };
    // The pill is the widest thing the song adds to the bar; anything else that moved beside it (a
    // reading shifted to make room) is narrower.
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

/// A left click at `(x, y)` .
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

/// The session draws the song's pill in the bar, and the pill opens as the song's card over the
/// desk: the pair is written as `{name}-pill.png` and `{name}-card.png` where `name` asks for a
/// picture.
fn pill_and_card(
    session: &mut Session,
    dir: &std::path::Path,
    name: Option<&str>,
    summary: Summary,
) {
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
    if let Some(name) = name {
        super::desk::capture_to(session, dir, &format!("{name}-pill"));
    }

    click_the_pill(session, &empty, &named, width);
    settle(session, 8);
    let (opened, _) = shot(session);
    assert!(
        changed(&named, &opened, width, BAR, desk_bottom()) > 1_000,
        "the card stands over the desk: {} pixels of it moved",
        changed(&named, &opened, width, BAR, desk_bottom())
    );
    if let Some(name) = name {
        super::desk::capture_to(session, dir, &format!("{name}-card"));
    }
}

/// A session on a display `width` wide, as a phone stands.
fn session_wide(width: i32) -> Session {
    crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", width, HEIGHT)],
        Options {
            collab: false,
            ..Options::default()
        },
    )
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
    pill_and_card(&mut session, &dir, Some("u2-dark"), song(COVER));
}

/// The same song with its player paused: the middle control offers to play it, so the card is not
/// the one a playing song draws.
#[test]
fn a_paused_song_offers_to_play_again() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut paused = session();
    pill_and_card(
        &mut paused,
        &dir,
        Some("u2-dark-paused"),
        song_with(COVER, false),
    );
    let (paused_card, width) = shot(&mut paused);

    let mut playing = session();
    pill_and_card(&mut playing, &dir, None, song(COVER));
    let (playing_card, _) = shot(&mut playing);

    let differs = changed(&playing_card, &paused_card, width, BAR, desk_bottom());
    assert!(
        differs > 20,
        "a paused song's card offers another control than a playing one's: {differs} pixels"
    );
}

/// A song whose player sends no cover: the card draws the song's glyph in the artwork's place, so
/// the row keeps its shape.
#[test]
fn a_song_without_a_cover_keeps_the_card_its_shape() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = session();
    pill_and_card(&mut session, &dir, Some("u2-dark-no-art"), song(""));
}

/// A phone's desk keeps the song in the pill, and expanding it drops the card full-width from the
/// bar.
#[test]
fn a_phone_names_the_song_in_the_pill_and_drops_its_card_from_the_bar() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = session_wide(PHONE);
    pill_and_card(&mut session, &dir, Some("u2-phone"), song(COVER));
}

/// The same on a phone drawing the light theme.
#[test]
fn a_phone_in_the_light_theme_drops_the_song_card_from_the_bar() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = light_session_wide(PHONE);
    pill_and_card(&mut session, &dir, Some("u2-phone-light"), song(COVER));
}

/// The song's pill and card on the light desk.
#[test]
fn the_song_stands_in_the_light_bar_and_opens_as_its_card() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = super::desk::light_session();
    pill_and_card(&mut session, &dir, Some("u2-light"), song(COVER));
}

/// How many pixels of `frame` inside `rect` stand in a colour `look` accepts: `rect` as
/// `(x, y, width, height)` in logical pixels.
fn cover_pixels(
    frame: &[u8],
    width: u32,
    rect: (i32, i32, i32, i32),
    look: impl Fn(u8, u8, u8) -> bool,
) -> usize {
    let stride = usize::try_from(width).expect("a display with columns") * 4;
    let (x0, y0, across, down) = rect;
    let mut count = 0;
    for y in usize::try_from(y0).expect("a row of the display")
        ..usize::try_from(y0.saturating_add(down)).expect("a row of the display")
    {
        for x in usize::try_from(x0).expect("a column of the display")
            ..usize::try_from(x0.saturating_add(across)).expect("a column of the display")
        {
            let at = y * stride + x * 4;
            let Some(pixel) = frame.get(at..at + 3) else {
                continue;
            };
            if look(pixel[0], pixel[1], pixel[2]) {
                count += 1;
            }
        }
    }
    count
}

/// The cover's darker half, as both the fixture picture and the picture Music sends paint it: a
/// purple no part of the desk or the panel's chrome is.
fn cover_purple(r: u8, g: u8, b: u8) -> bool {
    r < 120 && g < 85 && b > 125 && u32::from(b) > u32::from(r) + 40
}

/// The cover's lighter half in the fixture picture: its other colour, so both ends of the picture
/// prove the picture was painted and not one flat tone.
fn cover_warm(r: u8, g: u8, b: u8) -> bool {
    r > 220 && g > 180 && b < 160
}

/// Where two frames differ inside `rect` : the box around every pixel that moved, as
/// `(x, y, width, height)` .
fn box_differing(
    before: &[u8],
    after: &[u8],
    width: u32,
    rect: (i32, i32, i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    let stride = usize::try_from(width).expect("a display with columns") * 4;
    let (x0, y0, across, down) = rect;
    let (mut left, mut top, mut right, mut bottom) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in y0..y0.saturating_add(down) {
        for x in x0..x0.saturating_add(across) {
            let at = usize::try_from(y).expect("a row of the display") * stride
                + usize::try_from(x).expect("a column of the display") * 4;
            if before.get(at..at + 4) == after.get(at..at + 4) {
                continue;
            }
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
    }
    (left <= right).then(|| (left, top, right - left + 1, bottom - top + 1))
}

/// The part of the display under the bar, where the cards stand.
fn desk() -> (i32, i32, i32, i32) {
    (
        0,
        i32::try_from(BAR).expect("a bar of rows"),
        WIDTH,
        i32::try_from(desk_bottom() - BAR).expect("a display with rows"),
    )
}

/// The card paints the song's cover: the picture's own two colours stand in its artwork tile, and
/// nowhere on the desk it covers.
#[test]
fn the_card_paints_the_songs_cover() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let mut session = session();
    settle(&mut session, 8);
    let (empty, width) = shot(&mut session);

    show(&session, MUSIC, song(COVER));
    settle(&mut session, 8);
    let (named, _) = shot(&mut session);
    click_the_pill(&mut session, &empty, &named, width);
    settle(&mut session, 8);
    let (card, _) = shot(&mut session);

    let (purple, warm) = (
        cover_pixels(&card, width, desk(), cover_purple),
        cover_pixels(&card, width, desk(), cover_warm),
    );
    assert!(
        purple > 200 && warm > 200,
        "the cover's two colours stand in the card's artwork tile: {purple} purple, {warm} warm pixels"
    );
    assert_eq!(
        cover_pixels(&empty, width, desk(), cover_purple),
        0,
        "nothing on the desk paints the cover before the card opens"
    );
}

/// The cover the player publishes, as the audio service reads one: its size and its pixels.
fn cover() -> serde_json::Value {
    serde_json::json!({"width": 16, "height": 16, "rgba": "Oi6W/0Ezkf9JOIz/UD2I/1hCg/9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/0Ezkf9JOIz/UD2I/1hCg/9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/9JOIz/UD2I/1hCg/9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/UD2I/1hCg/9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/1hCg/9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf9fR37/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/Z0x6/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/25Rdf92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL/92VnD/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/fVts/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm/4VgZ/+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf+MZWL/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf/9sBz/lGpe/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf/9sBz//7UY/5tvWf+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf/9sBz//7UY//+6E/+jdFT/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf/9sBz//7UY//+6E///vw7/qnlQ/7J+S/+5g0b/wYhC/8iNPf/Qkjj/15c0/9+cL//moSr/7qYm//WrIf/9sBz//7UY//+6E///vw7//8QK/w=="})
}

/// The song a player publishes while it plays, as the audio service publishes it: its title, who it
/// is by, its cover, where the playhead stands and how long it is.
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

/// A session whose quick settings panel is the app this repository builds, a player publishing what
/// `published` holds, and what the panel asked it for.
fn panel_session(
    published: &super::lock::Publishing,
) -> (
    Session,
    tempfile::TempDir,
    std::rc::Rc<std::cell::RefCell<Vec<String>>>,
) {
    let home = super::lock::private_home();
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let panel = quick_settings::surface_of(tilo_test_env::sibling_binary(
        "tilo-quick-settings",
        "tilo-quick-settings",
    ));
    let session = crate::common::headless(Options {
        services: vec![Box::new(super::lock::PlayerService::new(
            std::rc::Rc::clone(&asked),
            published.clone(),
        ))],
        action_plane: tilo_session::ActionPlane::At(home.path().join("collab.sock")),
        surfaces: vec![panel],
        collab: false,
        ..Options::default()
    });
    (session, home, asked)
}

/// A pixel that carries colour: the panel's own chrome is grey, so colour in it comes from a
/// picture.
fn carries_colour(r: u8, g: u8, b: u8) -> bool {
    r.max(g).max(b) - r.min(g).min(b) >= 30
}

/// A cover-sized square holds at least this much colour once the panel paints the song's picture:
/// the whole square, less its rounded corners and the greys the gradient passes through.
const COVER_DRAWN: usize = 800;

/// How far into the panel the row's picture stands: the panel's padding and the picture's own
/// width.
const COVER_EDGE: usize = 64;

/// The most colour any cover-sized square at the panel's leading edge holds: the row puts the
/// player's picture there, so a square filled with colour is that picture, and a player that sent
/// none leaves every square under this bar.
fn cover_block(frame: &[u8], width: u32, rect: (i32, i32, i32, i32)) -> usize {
    const SIDE: usize = 40;
    let stride = usize::try_from(width).expect("a display with columns") * 4;
    let (x0, y0, across, down) = rect;
    let (across, down) = (
        usize::try_from(across).expect("a panel with columns"),
        usize::try_from(down).expect("a panel with rows"),
    );
    let (x0, y0) = (
        usize::try_from(x0).expect("a column of the display"),
        usize::try_from(y0).expect("a row of the display"),
    );
    // Where the colour stands, summed over every rectangle that starts at the panel's corner, so a
    // square's colour is four lookups.
    let mut summed = vec![0_usize; (down + 1) * (across + 1)];
    for y in 0..down {
        let mut row = 0;
        for x in 0..across {
            let at = (y0 + y) * stride + (x0 + x) * 4;
            let Some(pixel) = frame.get(at..at + 3) else {
                continue;
            };
            if carries_colour(pixel[0], pixel[1], pixel[2]) {
                row += 1;
            }
            summed[(y + 1) * (across + 1) + x + 1] = summed[y * (across + 1) + x + 1] + row;
        }
    }
    let square = |x: usize, y: usize| {
        let (below, above) = ((y + SIDE) * (across + 1), y * (across + 1));
        (summed[below + x + SIDE] + summed[above + x])
            .saturating_sub(summed[above + x + SIDE] + summed[below + x])
    };
    // Only the panel's leading edge is searched: that is where the row puts the player's picture,
    // and the panel keeps its own colour at its trailing edge, on the transport a press calls.
    let mut most = 0;
    for y in 0..down.saturating_sub(SIDE - 1) {
        for x in 0..across.saturating_sub(SIDE - 1).min(COVER_EDGE) {
            most = most.max(square(x, y));
        }
    }
    most
}

/// The panel paints the song's cover (`covered`), or none of it: the picture stands in the panel's
/// own window, read from the screen, so what is counted is what the panel drew and not what it says
/// it offers.
fn until_cover(
    session: &mut Session,
    width: u32,
    rect: (i32, i32, i32, i32),
    covered: bool,
) -> usize {
    crate::common::wait_for(
        session,
        if covered {
            "the panel paints the song's cover"
        } else {
            "the panel paints no cover"
        },
        |session| {
            let (drawn, _) = shot(session);
            let cover = cover_block(&drawn, width, rect);
            ((cover > COVER_DRAWN) == covered).then_some(cover)
        },
    )
}

/// Ask the panel to run `action` , as a person's press does.
fn ask_the_panel(session: &mut Session, action: &str) {
    let offered = session
        .surfaces()
        .published(quick_settings::SURFACE)
        .into_iter()
        .find(|offered| offered.action == action)
        .expect("the panel offers it");
    session
        .ask_surface(
            quick_settings::SURFACE,
            &offered.resource,
            &offered.action,
            ui::tilo_actions::remote::InvokeArgument::None,
        )
        .expect("the panel is running");
}

/// The panel draws the song that stands there before it opens, takes a new reading while it stands,
/// and steers the player through the transport it shows.
fn panel_names_the_song(name: &str, first: serde_json::Value, then: Option<serde_json::Value>) {
    let dir = shots_dir();
    let published = super::lock::Publishing::default();
    published.set_playing(Some(first));
    let (mut session, _home, asked) = panel_session(&published);
    settle(&mut session, 4);
    let (closed, width) = shot(&mut session);

    invoke(&mut session, CHROME_RESOURCE, "quick-settings");
    let arrived = crate::common::wait_for(
        &mut session,
        "the panel is drawn over the desk",
        |session| {
            session
                .surfaces()
                .running(quick_settings::SURFACE)
                .then_some(())?;
            let (drawn, _) = shot(session);
            box_differing(&closed, &drawn, width, desk())
                .filter(|(_, _, across, down)| *across > 200 && *down > 150)
        },
    );

    // The panel arriving is not the panel standing: read the row only once
    // the frame has come to rest, or a colour in flight is counted as drawn,
    // and read the panel's own box from the frame it came to rest at.
    settle(&mut session, 60);
    let (open, _) = shot(&mut session);
    let rect = box_differing(&closed, &open, width, desk())
        .filter(|(_, _, across, down)| *across > 200 && *down > 150)
        .unwrap_or(arrived);

    // The row is in the panel's own window: the picture the player sent stands at its leading edge,
    // and a player that sent none draws the row without one. Both cases are counted, so the picture
    // cannot be the panel's own chrome: the run without a cover paints none of its colour.
    let without = name.ends_with("no-art");
    let opens_with_cover = then.is_none() && !without;
    let cover = until_cover(&mut session, width, rect, opens_with_cover);
    assert_eq!(
        cover > COVER_DRAWN,
        opens_with_cover,
        "the panel paints the song's cover when it was sent one and none when it was not: {cover} coloured pixels in its cover square"
    );

    // A player that sends its cover while the panel stands reaches it: the picture appears in the
    // panel's own window, which the reading the panel opened with left bare.
    if let Some(then) = then {
        assert!(
            cover <= COVER_DRAWN,
            "the reading the panel opened with sent no cover, so any picture here is a new one"
        );
        published.set_playing(Some(then));
        let painted = until_cover(&mut session, width, rect, true);
        assert!(
            painted > COVER_DRAWN,
            "the reading published while the panel stands reaches it: {painted} coloured pixels in its cover square"
        );
        settle(&mut session, 8);
    }

    super::desk::capture_to(&mut session, &dir, name);

    // A press on the transport is the panel's action running on the player.
    ask_the_panel(&mut session, "play-pause");
    crate::common::wait_for(&mut session, "the player is asked", |_| {
        asked.borrow().last().cloned()
    });
    assert_eq!(
        asked.borrow().as_slice(),
        ["play-pause"],
        "the panel's transport asked the player once"
    );
}

/// The song Quick Settings names, on the dark desk: the panel opens on a song
/// whose player has not sent its cover, and the cover arrives while it stands,
/// so the picture in its window came from a reading published after it opened.
#[test]
fn quick_settings_names_the_song_that_plays() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    panel_names_the_song(
        "u2-quick-settings",
        song_without_a_cover(),
        Some(playing_song()),
    );
}

/// The same song paused when the panel opens, and one whose player has not sent its cover: the
/// middle control offers to play, and the artwork stands as the song's glyph.
#[test]
fn quick_settings_names_a_paused_song_and_one_without_a_cover() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    panel_names_the_song("u2-quick-settings-paused", paused_song(), None);
    panel_names_the_song("u2-quick-settings-no-art", song_without_a_cover(), None);
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

/// How bright a frame is, of 255: the light theme draws the screen light, the dark one dark.
fn brightness(frame: &[u8]) -> f64 {
    let (sum, count) = frame
        .chunks_exact(4)
        .fold((0.0, 0.0), |(sum, count), pixel| {
            let grey = (f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2])) / 3.0;
            (sum + grey, count + 1.0)
        });
    sum / count
}

/// The song Quick Settings names, in the light theme: its card over the panel's page, beside the
/// same panel with nothing playing.
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
    std::fs::copy(
        beside.path().join("now-playing.png"),
        dir.join("u2-quick-settings-light.png"),
    )
    .expect("the capture is written");
}

/// The lock screen as it stands with `song` playing: its card with the cover and the playhead,
/// drawn by the screen itself.
fn lock_frame(
    song: serde_json::Value,
    media: fn(&serde_json::Value) -> bool,
) -> tilo_compositor::Screenshot {
    let mut locked = super::lock::Locked::new();
    let quiet = locked.quiet.clone();
    locked.plays(Some(song), media, &quiet)
}

/// The lock screen's card shows the song that plays, its cover and where the playhead stands.
fn lock_card(name: &str, song: serde_json::Value, media: fn(&serde_json::Value) -> bool) {
    let dir = shots_dir();
    let playing = lock_frame(song, media);
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

/// The lock screen draws in the language the person chose: the same screen in German is another
/// picture than the English one, written beside it for the words to be read.
#[test]
fn the_lock_screen_reads_in_the_persons_language() {
    let _settled = ui::motion::testing::settled();
    let dir = shots_dir();
    let said = |value: &serde_json::Value| value["title"] == "Glasslight";
    let english = lock_frame(playing_song(), said);
    settings_store::appearance::LANGUAGE
        .set(Some("de-DE".to_owned()))
        .expect("the person's language is saved");
    let german = lock_frame(playing_song(), said);
    settings_store::appearance::LANGUAGE
        .set(None)
        .expect("the language is the system's again");
    let (width, height) = (german.width, german.height);
    let reads = changed(&english.rgba, &german.rgba, width, 0, height);
    assert!(
        reads > 500,
        "the screen is drawn in the person's language: {reads} pixels differ from the English one"
    );
    std::fs::create_dir_all(&dir).expect("create the shots directory");
    image::save_buffer(
        dir.join("u2-lock-de.png"),
        &german.rgba,
        german.width,
        german.height,
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

/// The same song whose player has not sent its cover: the artwork stands as the song's glyph.
#[test]
fn the_lock_screen_shows_a_song_without_a_cover() {
    let _settled = ui::motion::testing::settled();
    lock_card("u2-lock-no-art", song_without_a_cover(), |media| {
        media["title"] == "Glasslight" && media["artwork"] == ""
    });
}

/// The song the lock screen shows, in the light theme: its card beside the same screen with nothing
/// playing.
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
    std::fs::copy(
        beside.path().join("playing.png"),
        dir.join("u2-lock-light.png"),
    )
    .expect("the capture is written");
}
