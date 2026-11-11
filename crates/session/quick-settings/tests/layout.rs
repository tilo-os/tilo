//! The panel as a person meets it, headless: what each page shows, where
//! things sit, and which effect each control invokes on the device.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::rc::Rc;

use tilo_quick_settings::host::{FixtureHost, Lent};
use tilo_quick_settings::{
    Page, QsEffect, QsSection, QuickSettings, QuickSettingsApp, RowPart, scenarios,
};
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (360, 900);

fn staged(scenario: &str) -> HeadlessApp<QuickSettingsApp> {
    HeadlessApp::<QuickSettingsApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

/// The panel on a desktop: the scenario's readings, and the host it records
/// what it is asked to do on.
fn hosted(scenario: &str) -> (HeadlessApp<QuickSettingsApp>, Rc<FixtureHost>) {
    let host = Rc::new(FixtureHost::new(scenarios::readings(Some(scenario))));
    let lent: Rc<dyn tilo_quick_settings::host::PanelHost> = host.clone();
    let mut app =
        HeadlessApp::<QuickSettingsApp>::for_person_with_lent("person", Lent(lent)).unwrap();
    for invoke in scenarios::actions(Some(scenario)) {
        app.invoke(invoke).unwrap();
    }
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, host)
}

/// The panel's own surface: the card fills the window the desk gives a
/// popover, so the app's root area is the card's rect.
fn card(app: &mut HeadlessApp<QuickSettingsApp>) -> Rect {
    target(app, "BoundComponent[0]/ResourceApp[0]")
}

fn target(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) -> Rect {
    let targets = app.harness().targets();
    *targets.get(name).unwrap_or_else(|| {
        let mut names: Vec<&String> = targets.keys().collect();
        names.sort();
        panic!("no target named {name}; there are {names:?}")
    })
}

/// Press `name` as a person does, and let the panel answer.
fn click(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) {
    app.harness().click_named(name).unwrap();
    app.settle();
}

/// Press a tile's chevron, at the tile's trailing end.
fn open_details(app: &mut HeadlessApp<QuickSettingsApp>, tile: &str) {
    let rect = target(app, tile);
    app.harness().click_at(rect.x1 - 8.0, rect.center().y);
    app.settle();
}

fn has(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) -> bool {
    app.harness().targets().contains_key(name)
}

#[test]
fn the_controls_page_has_the_tiles_two_to_a_row_then_the_sliders_then_the_rows_and_the_footer() {
    let mut app = staged("controls");
    let brightness = target(&mut app, "qs-brightness-row");
    let volume = target(&mut app, "qs-volume-row");
    let wifi = target(&mut app, "qs-tile-wifi");
    let bluetooth = target(&mut app, "qs-tile-bluetooth");
    let vpn = target(&mut app, "qs-tile-vpn");
    let power_mode = target(&mut app, "qs-tile-power_mode");
    let battery = target(&mut app, "qs-row-battery");
    let notifications = target(&mut app, "qs-row-notifications");
    let power = target(&mut app, "qs-power");
    let account = target(&mut app, "qs-account");
    let card = card(&mut app);
    assert!(
        card.width() <= f64::from(VIEWPORT.0) && card.width() > f64::from(VIEWPORT.0) - 1.0,
        "the panel takes its window and never stands past it: {card:?}"
    );
    assert!(
        power.x1 <= card.x1 && power.x0 >= card.x0,
        "its controls stay on it"
    );
    assert!(
        (wifi.y0 - bluetooth.y0).abs() < 1.0 && wifi.x1 <= bluetooth.x0,
        "two tiles to a row: {wifi:?} {bluetooth:?}"
    );
    assert!(
        (wifi.height() - bluetooth.height()).abs() < 1.0,
        "the two tiles of a row stand the same height: {wifi:?} {bluetooth:?}"
    );
    assert!(
        bluetooth.height() > power_mode.height() + 1.0,
        "the state line that does not fit its tile breaks onto a second line, \
         and the tile grows for it: {bluetooth:?} {power_mode:?}"
    );
    assert!(vpn.y0 >= wifi.y1, "the third tile starts the next row");
    assert!(brightness.y0 >= vpn.y1, "the sliders come under the tiles");
    assert!(volume.y0 >= brightness.y1, "brightness, then volume");
    assert!(battery.y0 >= volume.y1, "the rows come under the sliders");
    assert!(notifications.y0 > battery.y0);
    assert!(power.y0 >= notifications.y1, "the footer comes last");
    assert!(
        !has(&mut app, "qs-section-wifi"),
        "nothing is open when the panel opens"
    );
    // One inset: what a page holds shares one left and one right edge, the
    // footer's controls with it. (A row's own box bleeds over the page's
    // padding, so its background reaches the card's edge; what it says
    // starts on the page's edge like the rest.)
    for (name, rect) in [
        ("a tile", wifi),
        ("a slider", brightness),
        ("the account", account),
    ] {
        assert!(
            (rect.x0 - wifi.x0).abs() < 0.5,
            "{name} starts on the page's left edge: {rect:?} against {wifi:?}"
        );
    }
    assert!(
        (brightness.x1 - (card.x1 - (wifi.x0 - card.x0))).abs() < 0.5,
        "a slider ends on the page's right edge: {brightness:?} in {card:?}"
    );
    assert!(
        (power.x1 - (card.x1 - (wifi.x0 - card.x0))).abs() < 0.5,
        "the footer ends on the page's right edge: {power:?} in {card:?}"
    );
    // The power mode is chosen on its tile and its page opens from there:
    // no row says it twice.
    assert!(
        !has(&mut app, "qs-row-power-mode"),
        "one power control, the tile"
    );
    assert!(has(&mut app, "qs-tile-power_mode"));
    // One compact row per slider: its icon, its track and its value, with
    // the device under the track it belongs to.
    assert!(
        brightness.height() <= 48.0,
        "a slider is one row, not two: {brightness:?}"
    );
    let device = target(&mut app, "qs-volume-device");
    assert!(
        device.y0 >= volume.y0 - 0.5 && device.y1 <= volume.y1 + 0.5,
        "the device is said on the slider's own row: {device:?} in {volume:?}"
    );
}

/// A detail page closes without leaving room behind: what stood under it
/// rests exactly where it stood before the section opened.
/// A section opens its own page in the controls' place, and Back returns
/// the controls to exactly where they stood.
#[test]
fn a_section_replaces_the_controls_and_back_returns_them_as_they_stood() {
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("controls");
    let anchors = [
        "qs-tile-vpn",
        "qs-brightness-row",
        "qs-volume-row",
        "qs-row-battery",
    ];
    let before: Vec<Rect> = anchors.iter().map(|name| target(&mut app, name)).collect();
    open_details(&mut app, "qs-tile-wifi");
    assert!(has(&mut app, "qs-section-wifi"), "the networks show");
    assert!(
        !has(&mut app, "qs-tile-vpn"),
        "and the controls are gone: the page replaces them"
    );
    assert_eq!(
        host.showing().map(|showing| showing.section),
        Some(Some(QsSection::Wifi))
    );
    click(&mut app, "qs-back");
    assert!(!has(&mut app, "qs-section-wifi"));
    assert!(has(&mut app, "qs-tile-vpn"), "Back returns the controls");
    for (name, was) in anchors.iter().zip(before) {
        let now = target(&mut app, name);
        assert!(
            (now.y0 - was.y0).abs() < 0.5 && (now.y1 - was.y1).abs() < 0.5,
            "{name} rests where it stood: was {was:?}, now {now:?}"
        );
    }
}

/// Every section opens a page with its own name and Back, in the page slot
/// the controls stood in.
#[test]
fn every_section_opens_its_page_and_back_returns_the_controls() {
    let _settled = ui::motion::testing::settled();
    for (tile, section, title, shows) in [
        (
            "qs-tile-wifi",
            QsSection::Wifi,
            "Wi-Fi networks",
            "qs-network-Home",
        ),
        (
            "qs-tile-bluetooth",
            QsSection::Bluetooth,
            "Bluetooth devices",
            "qs-accessory-00:1B:66:A1:B2:C3",
        ),
        (
            "qs-tile-power_mode",
            QsSection::PowerMode,
            "Power mode",
            "qs-section-power-mode",
        ),
    ] {
        let (mut app, host) = hosted("controls");
        let top = target(&mut app, "qs-tile-wifi").y0;
        open_details(&mut app, tile);
        assert_eq!(
            host.showing().map(|showing| showing.section),
            Some(Some(section)),
            "{tile} opens {section:?}"
        );
        assert!(has(&mut app, shows), "{section:?} lists what it is about");
        assert_eq!(
            row_words(&mut app, "qs-page-title").and_then(|said| said.0),
            Some(title.to_owned())
        );
        let back = target(&mut app, "qs-back");
        assert!(
            (back.y0 - top).abs() < 0.5,
            "{title}'s page stands in the page slot: {back:?} against {top}"
        );
        click(&mut app, "qs-back");
        assert!(has(&mut app, "qs-tile-vpn"), "Back returns the controls");
        assert_eq!(
            host.showing().map(|showing| showing.section),
            Some(None),
            "and the host reads the controls"
        );
    }
}

/// The whole panel answers the keyboard: Tab to a tile, Enter toggles it,
/// Right reaches its details, Enter opens their page and Back returns.
#[test]
fn the_keyboard_toggles_a_tile_reaches_its_details_and_opens_them() {
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("controls");
    app.harness().tab(false);
    assert_eq!(
        key_on(&mut app).as_deref(),
        Some("Wi-Fi"),
        "the first Tab reaches the first tile"
    );
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        host.ran(),
        [QsEffect::SetWifi { on: false }],
        "Enter throws the switch under the keyboard"
    );
    app.harness().key("ArrowRight");
    app.settle();
    assert_eq!(
        key_on(&mut app).as_deref(),
        Some("Wi-Fi networks"),
        "Right reaches the tile's details"
    );
    app.harness().key("ArrowLeft");
    app.settle();
    assert_eq!(
        key_on(&mut app).as_deref(),
        Some("Wi-Fi"),
        "Left comes back to the switch"
    );
    app.harness().key("ArrowRight");
    app.settle();
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        host.showing().map(|showing| showing.section),
        Some(Some(QsSection::Wifi)),
        "Enter opens the networks' page"
    );
    assert!(has(&mut app, "qs-section-wifi") && has(&mut app, "qs-network-Office"));
    assert!(!has(&mut app, "qs-tile-vpn"), "in the controls' place");
    click(&mut app, "qs-back");
    assert!(!has(&mut app, "qs-section-wifi"));
    open_details(&mut app, "qs-tile-wifi");
    assert_eq!(
        host.showing().map(|showing| showing.section),
        Some(Some(QsSection::Wifi)),
        "and a press on its chevron opens it again"
    );
}

#[test]
fn a_tiles_chevron_opens_its_page_and_the_host_reads_it() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("controls");
    open_details(&mut app, "qs-tile-wifi");
    let back = target(&mut app, "qs-back");
    let section = target(&mut app, "qs-section-wifi");
    assert!(
        back.y0 < section.y0,
        "the page's Back stands above what it lists: {back:?} {section:?}"
    );
    assert_eq!(
        host.showing().map(|showing| showing.section),
        Some(Some(QsSection::Wifi)),
        "the host reads the networks while their page stands"
    );
    // The networks: the one in use first, then the known ones.
    let home = target(&mut app, "qs-network-Home");
    let office = target(&mut app, "qs-network-Office");
    assert!(home.y1 <= office.y0);
    click(&mut app, "qs-back");
    assert!(
        !has(&mut app, "qs-section-wifi"),
        "Back returns the controls"
    );
}

#[test]
fn a_section_page_stands_in_the_page_slot_of_the_controls() {
    let (mut app, _host) = hosted("controls");
    let top = target(&mut app, "qs-tile-wifi").y0;
    // The clock stands still where the page changes: the slide paints the
    // two pages on their way, and neither page is laid out anywhere else.
    let frozen = ui::motion::testing::frozen();
    open_details(&mut app, "qs-tile-wifi");
    let back = target(&mut app, "qs-back");
    assert!(
        (back.y0 - top).abs() < 0.5,
        "the page it opens stands where the controls stood: {back:?} against {top}"
    );
    assert!(
        has(&mut app, "qs-section-wifi"),
        "and it lists the networks"
    );
    drop(frozen);
    let _settled = ui::motion::testing::settled();
    app.settle();
    assert!(!has(&mut app, "qs-tile-vpn"), "the controls stay gone");
}

#[test]
fn each_control_invokes_its_effect_on_the_device() {
    let (mut app, host) = hosted("controls");
    for name in [
        "qs-tile-wifi",
        "qs-tile-bluetooth",
        "qs-tile-night_light",
        "qs-volume-mute",
    ] {
        click(&mut app, name);
    }
    assert_eq!(
        host.ran(),
        [
            QsEffect::SetWifi { on: false },
            QsEffect::SetBluetooth { on: false },
            QsEffect::SetNightLight { on: true },
            QsEffect::SetMuted { muted: true },
        ]
    );
}

#[test]
fn what_plays_is_steered_from_the_panel_through_its_effects() {
    let (mut app, _) = hosted("controls");
    assert!(!has(&mut app, "qs-now-playing"), "nothing plays, no card");

    let (mut app, host) = hosted("now-playing");
    assert!(
        has(&mut app, "qs-now-playing-player"),
        "two players to choose from"
    );
    assert!(has(&mut app, "qs-now-playing-output"));
    for name in [
        "qs-now-playing-toggle",
        "qs-now-playing-next",
        "qs-now-playing-previous",
    ] {
        click(&mut app, name);
    }
    assert!(
        has(&mut app, "qs-now-playing-seek"),
        "the song's point stands as a slider the person drags"
    );
    click(&mut app, "qs-now-playing-seek");
    let mut ran = host.ran();
    assert_eq!(
        &ran[..3],
        [
            QsEffect::PlayPause,
            QsEffect::NextTrack,
            QsEffect::PreviousTrack,
        ]
    );
    let Some(QsEffect::Seek { fraction }) = ran.pop() else {
        panic!("pressing the slider asks the player to seek: {ran:?}");
    };
    assert!(
        (fraction - 0.5).abs() < 0.02,
        "the middle of the slider is the middle of the song: {fraction}"
    );
    assert_eq!(ran.len(), 3, "one seek, from one press");

    // The bar's ends name where the song stands and what it leaves, as the
    // card's bar does, and the whole bar belongs to the song's block: it spans
    // the song's own width, stands under its lines and transport, and keeps
    // apart from the pickers that follow.
    let song = target(&mut app, "qs-now-playing");
    let row = target(&mut app, "qs-now-playing-seek-row");
    let bar = target(&mut app, "qs-now-playing-seek");
    let elapsed = target(&mut app, "qs-now-playing-elapsed");
    let remaining = target(&mut app, "qs-now-playing-remaining");
    let players = target(&mut app, "qs-now-playing-player");
    assert!(
        elapsed.x1 <= bar.x0 + 0.5,
        "where the song stands stands at the bar's leading end: {elapsed:?} before {bar:?}"
    );
    assert!(
        remaining.x0 >= bar.x1 - 0.5,
        "what is left stands at its trailing end: {remaining:?} after {bar:?}"
    );
    for (name, label) in [("elapsed", elapsed), ("remaining", remaining)] {
        assert!(
            (label.center().y - bar.center().y).abs() < 1.0,
            "the {name} label reads on the bar's line: {label:?} beside {bar:?}"
        );
    }
    assert!(
        (row.x0 - song.x0).abs() < 0.5 && (row.x1 - song.x1).abs() < 0.5,
        "the bar spans the song's row: {row:?} beside {song:?}"
    );
    assert!(
        song.y1 <= row.y0 && row.y1 <= players.y0,
        "the bar stands under the song and above the pickers: {song:?} {row:?} {players:?}"
    );
    assert!(
        row.y0 - song.y1 < players.y0 - row.y1,
        "the bar stays with the song's block, apart from the pickers: {row:?} under {song:?}, {players:?} below"
    );
}

/// One player and one place to hear it leave nothing to choose, so the row
/// asks nothing: the song's own name is the way back to the player.
#[test]
fn one_player_and_one_output_leave_no_picker_and_the_name_opens_the_app() {
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("now-playing");
    host.update(|state| {
        let playing = state
            .now_playing
            .as_mut()
            .expect("the fixture plays a song");
        playing.players.retain(|player| player.active);
        playing.outputs.retain(|output| output.current);
    });
    app.settle();
    for picker in [
        "qs-now-playing-player",
        "qs-now-playing-output",
        "qs-now-playing-source",
    ] {
        assert!(
            !has(&mut app, picker),
            "the row asks nothing it cannot choose: {picker} stands"
        );
    }
    click(&mut app, "qs-now-playing-open");
    assert_eq!(
        host.ran(),
        [QsEffect::OpenPlaying {
            app: "so.tilo.music".into()
        }],
        "the song's name opens the app that plays it"
    );
}

/// The song stands in the panel as a row a person reads: the cover it came
/// with at the leading edge, the song and who it is by beside it, and the
/// transport named by what each button does. A reading that lands while the
/// panel is open is what the row shows next.
#[test]
fn the_song_stands_in_the_row_with_its_cover_and_named_transport() {
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("now-playing");
    assert!(
        has(&mut app, "qs-now-playing"),
        "the song stands in the panel"
    );
    let cover = target(&mut app, "qs-now-playing-cover");
    assert!(
        (cover.width() - 40.0).abs() < 0.5 && (cover.height() - 40.0).abs() < 0.5,
        "the cover takes its square at the row's leading edge: {cover:?}"
    );
    let name = target(&mut app, "qs-now-playing-open");
    assert!(
        name.x0 > cover.x0 + cover.width() / 2.0 && name.y0 < cover.y1 && name.y1 > cover.y0,
        "the song's name stands in the row beside the cover: {name:?} against {cover:?}"
    );
    let said_by = |app: &mut HeadlessApp<QuickSettingsApp>| {
        ui::headless::test_support::semantic_nodes(app.harness())
            .into_iter()
            .filter_map(|(_, node)| node.name)
            .collect::<Vec<_>>()
    };
    let names = said_by(&mut app);
    for said in [
        "Clair de lune",
        "Claude Debussy",
        "Pause",
        "Previous song",
        "Next song",
    ] {
        assert!(
            names.iter().any(|name| name == said),
            "the row says {said:?}: {names:?}"
        );
    }

    // The player pauses while the panel stands: the middle control now says
    // what a press would do.
    host.update(|state| {
        state
            .now_playing
            .as_mut()
            .expect("the fixture plays a song")
            .playing = false;
    });
    app.settle();
    let names = said_by(&mut app);
    assert!(
        names.iter().any(|name| name == "Play"),
        "a paused song's middle control offers to play: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "Pause"),
        "and no longer offers to pause: {names:?}"
    );
}

#[test]
fn the_effects_are_published_for_the_palette_scripts_and_agents() {
    let (mut app, host) = hosted("controls");
    app.effect(&QsEffect::SetVolume { level: 0.25 }).unwrap();
    app.effect(&QsEffect::SetPowerMode {
        mode: tilo_quick_settings::PowerProfile::Saver,
    })
    .unwrap();
    assert_eq!(
        host.ran(),
        [
            QsEffect::SetVolume { level: 0.25 },
            QsEffect::SetPowerMode {
                mode: tilo_quick_settings::PowerProfile::Saver
            },
        ]
    );
}

#[test]
fn a_protected_network_never_joined_asks_for_its_password_under_it() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("wifi");
    click(&mut app, "qs-network-Cafe guest");
    assert_eq!(
        host.ran(),
        [QsEffect::AskWifiPassword {
            network: "Cafe guest".into()
        }]
    );
    let mut app = staged("wifi-password");
    let network = target(&mut app, "qs-network-Cafe guest");
    let prompt = target(&mut app, "qs-wifi-password");
    let next = target(&mut app, "qs-network-Library");
    assert!(
        prompt.y0 >= network.y1 && next.y0 >= prompt.y1,
        "{prompt:?}"
    );
    assert!(
        has(&mut app, "qs-wifi-password-error"),
        "a wrong password says so"
    );
}

#[test]
fn typing_the_password_and_pressing_connect_joins_with_it() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("wifi-password");
    let field = app
        .harness()
        .targets()
        .into_keys()
        .find(|name| name.starts_with("qs-wifi-password-") && name.ends_with(char::is_numeric))
        .expect("the password field");
    app.harness().click_named(&field).unwrap();
    app.harness().type_text("hunter22");
    click(&mut app, "qs-wifi-password-connect");
    assert_eq!(
        host.ran(),
        [QsEffect::ConnectWifi {
            network: "Cafe guest".into(),
            password: "hunter22".into(),
        }]
    );
}

#[test]
fn opened_to_paste_the_panel_is_the_clipboard_alone_and_typing_searches_it() {
    let (mut app, host) = hosted("paste");
    assert!(!has(&mut app, "qs-volume"), "no controls while pasting");
    assert!(has(&mut app, "qs-copy-a1") && has(&mut app, "qs-copy-c3"));
    app.harness().click_named("qs-clipboard-search").unwrap();
    app.harness().type_text("grocery");
    app.settle();
    assert!(has(&mut app, "qs-copy-c3"));
    assert!(
        !has(&mut app, "qs-copy-a1"),
        "a copy that does not match goes"
    );
    click(&mut app, "qs-copy-c3");
    assert_eq!(host.ran(), [QsEffect::PasteCopy { copy: "c3".into() }]);
}

#[test]
fn a_paired_device_is_forgotten_from_its_own_button() {
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("bluetooth");
    click(&mut app, "qs-bluetooth-forget-00:1B:66:D4:E5:F6");
    assert!(host.ran().is_empty(), "a pairing is not dropped unasked");
    assert!(
        app.confirm_asked().is_some(),
        "the person agrees to forget it"
    );
    app.settle();
    assert_eq!(
        host.ran(),
        [QsEffect::ForgetBluetooth {
            address: "00:1B:66:D4:E5:F6".into(),
        }],
        "the device's own button forgets it"
    );
}

#[test]
fn a_copy_is_pinned_and_removed_from_its_own_buttons() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("clipboard");
    click(&mut app, "qs-copy-b2-pin");
    click(&mut app, "qs-copy-a1-remove");
    assert_eq!(
        host.ran(),
        [
            QsEffect::PinCopy {
                copy: "b2".into(),
                pinned: true
            },
            QsEffect::RemoveCopy { copy: "a1".into() },
        ]
    );
}

#[test]
fn a_hidden_network_is_joined_by_the_name_and_password_typed_in_its_form() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, row) = hosted("wifi");
    click(&mut app, "qs-wifi-hidden");
    assert_eq!(
        row.ran(),
        [QsEffect::AskHiddenNetwork],
        "the row asks for the name and password"
    );

    let (mut app, host) = hosted("wifi-hidden");
    let field = |app: &mut HeadlessApp<QuickSettingsApp>, prefix: &str| {
        app.harness()
            .targets()
            .into_keys()
            .find(|name| name.starts_with(prefix) && name.ends_with(char::is_numeric))
            .unwrap_or_else(|| panic!("the {prefix} field"))
    };
    let name = field(&mut app, "qs-hidden-network-name-");
    app.harness().click_named(&name).unwrap();
    app.harness().type_text("Basement");
    app.settle();
    let password = field(&mut app, "qs-hidden-network-password-");
    assert_ne!(password, name, "the name and the password are two fields");
    app.harness().click_named(&password).unwrap();
    app.harness().type_text("hunter22");
    click(&mut app, "qs-hidden-network-connect");
    assert_eq!(
        host.ran(),
        [QsEffect::ConnectHiddenWifi {
            network: "Basement".into(),
            password: "hunter22".into(),
        }]
    );
}

#[test]
fn a_devices_row_says_which_of_its_buttons_was_pressed() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("devices");
    click(&mut app, "qs-device-speaker-request-allow");
    click(&mut app, "qs-device-phone-control");
    assert_eq!(
        host.ran(),
        [
            QsEffect::PressDevice {
                row: "speaker-request".into(),
                part: RowPart::Allow,
            },
            QsEffect::PressDevice {
                row: "phone".into(),
                part: RowPart::Control,
            },
        ]
    );
}

#[test]
fn an_accessory_moves_to_a_device_in_reach_and_disconnects_from_here() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("accessory");
    // This laptop has the accessory and the tablet is out of reach: both are
    // there to be read, and neither takes the press.
    for device in ["tablet", "laptop", "desk"] {
        assert!(has(&mut app, &format!("qs-accessory-to-{device}")));
    }
    for device in ["tablet", "laptop"] {
        click(&mut app, &format!("qs-accessory-to-{device}"));
    }
    assert!(host.ran().is_empty(), "only a device in reach takes it");
    click(&mut app, "qs-accessory-to-desk");
    assert_eq!(
        host.ran(),
        [QsEffect::HandOffBluetooth {
            address: "00:1B:66:A1:B2:C3".into(),
            device: "desk".into(),
        }],
        "the desk is online and paired: the accessory moves there"
    );
    click(&mut app, "qs-accessory-here");
    assert_eq!(
        host.ran(),
        [
            QsEffect::HandOffBluetooth {
                address: "00:1B:66:A1:B2:C3".into(),
                device: "desk".into(),
            },
            QsEffect::DisconnectBluetooth {
                address: "00:1B:66:A1:B2:C3".into(),
            },
        ]
    );
}

/// A press the device or the session refuses says why over the page it
/// belongs to, and stays until the person puts it away.
#[test]
fn a_refusal_stands_over_the_page_until_the_person_puts_it_away() {
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("controls");
    assert!(!has(&mut app, "qs-refusal"), "nothing is refused yet");
    host.update(|state| state.refusal = Some("the lock did not answer".into()));
    app.settle();
    let refusal = target(&mut app, "qs-refusal");
    let tile = target(&mut app, "qs-tile-wifi");
    let card = card(&mut app);
    assert!(refusal.y1 <= tile.y0, "the reason stands over the page");
    assert!(
        refusal.x0 >= card.x0 && refusal.x1 <= card.x1,
        "the reason stays inside the card: {refusal:?} in {card:?}"
    );
    assert!(has(&mut app, "qs-refusal-dismiss"));
    click(&mut app, "qs-refusal-dismiss");
    app.settle();
    assert_eq!(host.ran(), [QsEffect::DismissRefusal]);
    assert!(!has(&mut app, "qs-refusal"), "put away");
}

#[test]
fn arranging_hides_adds_and_moves_controls_but_never_wifi() {
    let (mut app, _host) = hosted("edit");
    assert!(has(&mut app, "qs-edit-wifi"));
    app.effect(&QsEffect::HideControl {
        control: "wifi".into(),
    })
    .unwrap();
    app.effect(&QsEffect::HideControl {
        control: "vpn".into(),
    })
    .unwrap();
    app.settle();
    assert!(
        has(&mut app, "qs-edit-wifi"),
        "Wi-Fi is how a desktop gets online"
    );
    assert!(!has(&mut app, "qs-edit-vpn"));
    assert!(
        has(&mut app, "qs-edit-add-vpn"),
        "a hidden control can come back"
    );
    app.effect(&QsEffect::MoveControl {
        control: "bluetooth".into(),
        to: 0,
    })
    .unwrap();
    app.settle();
    let bluetooth = target(&mut app, "qs-edit-bluetooth");
    let wifi = target(&mut app, "qs-edit-wifi");
    assert!(bluetooth.y1 <= wifi.y0, "moved to the top");
    click(&mut app, "qs-edit-done");
    assert!(has(&mut app, "qs-tile-bluetooth") && !has(&mut app, "qs-tile-vpn"));
}

/// The control the keyboard is on, as assistive technology reads it.
fn focused(app: &mut HeadlessApp<QuickSettingsApp>) -> Option<ui::a11y::AccessNode> {
    app.harness().focused_access_node()
}

/// The name the keyboard is on: a tile is a group whose focused half holds
/// the name, so the deepest focused node is the one that answers the keys.
fn key_on(app: &mut HeadlessApp<QuickSettingsApp>) -> Option<String> {
    fn deepest(node: ui::a11y::AccessNode) -> Option<String> {
        if node.state.focused {
            return node.name;
        }
        node.children.into_iter().find_map(deepest)
    }
    focused(app).and_then(deepest)
}

/// Tab until the keyboard is on the arranged row named `name`.
fn tab_to_row(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) {
    for _ in 0..40 {
        app.harness().tab(false);
        if focused(app).is_some_and(|node| {
            node.role == ui::a11y::Role::ListItem && node.name.as_deref() == Some(name)
        }) {
            return;
        }
    }
    panic!("Tab never reaches the {name} row");
}

/// Press `key` with Ctrl held, and let the panel answer.
fn ctrl(app: &mut HeadlessApp<QuickSettingsApp>, key: &str) {
    app.harness().set_modifiers(&["ctrl".to_owned()]);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
    app.settle();
}

#[test]
fn ctrl_up_and_down_move_the_focused_control_and_it_keeps_the_keyboard() {
    let (mut app, host) = hosted("edit");
    tab_to_row(&mut app, "Bluetooth");
    assert!(
        focused(&mut app)
            .and_then(|node| node.description)
            .is_some_and(|description| description.contains("Control plus Arrow keys")),
        "the row says how to move it"
    );
    ctrl(&mut app, "ArrowUp");
    assert_eq!(
        host.ran(),
        [QsEffect::MoveControl {
            control: "bluetooth".into(),
            to: 0,
        }]
    );
    let bluetooth = target(&mut app, "qs-edit-bluetooth");
    let wifi = target(&mut app, "qs-edit-wifi");
    assert!(bluetooth.y1 <= wifi.y0, "moved to the top");
    assert_eq!(
        focused(&mut app).and_then(|node| node.name).as_deref(),
        Some("Bluetooth"),
        "the keyboard stays on the control it moved"
    );
    ctrl(&mut app, "ArrowUp");
    assert_eq!(
        host.ran().len(),
        1,
        "the first control has nowhere up to go"
    );
    ctrl(&mut app, "ArrowDown");
    assert_eq!(
        host.ran().last(),
        Some(&QsEffect::MoveControl {
            control: "bluetooth".into(),
            to: 1,
        })
    );
    let bluetooth = target(&mut app, "qs-edit-bluetooth");
    let wifi = target(&mut app, "qs-edit-wifi");
    assert!(wifi.y1 <= bluetooth.y0, "back under Wi-Fi");
    assert_eq!(
        focused(&mut app).and_then(|node| node.name).as_deref(),
        Some("Bluetooth")
    );
}

#[test]
fn a_control_dragged_part_way_waits_for_the_release_and_lands_there() {
    let (mut app, host) = hosted("edit");
    let bluetooth = target(&mut app, "qs-edit-bluetooth");
    let (x, y) = (bluetooth.x0 + 20.0, bluetooth.center().y);
    let travel = bluetooth.height() * 1.6;
    let harness = app.harness();
    harness.send(&ui::synthetic_input::cursor_moved(x, y));
    harness.send(&ui::synthetic_input::mouse_press());
    for step in 1..=4_u8 {
        harness.send(&ui::synthetic_input::cursor_moved(
            x,
            y + travel * f64::from(step) / 4.0,
        ));
    }
    app.settle();
    assert!(
        host.ran().is_empty(),
        "nothing moves while the control is held"
    );
    app.harness().send(&ui::synthetic_input::mouse_release());
    app.settle();
    assert_eq!(
        host.ran(),
        [QsEffect::MoveControl {
            control: "bluetooth".into(),
            to: 2,
        }],
        "let go past the next control, it lands after it"
    );
}

#[test]
fn back_closes_a_section_leaves_a_page_and_finishes_arranging() {
    let mut panel = QuickSettings::default();
    panel.open_section(QsSection::Bluetooth);
    panel.show_accessory("00:11".into());
    assert_eq!(panel.page, Page::Accessory);
    panel.back();
    assert_eq!(
        (panel.page, panel.section),
        (Page::Root, Some(QsSection::Bluetooth))
    );
    panel.back();
    assert_eq!(panel.section, None);
    assert!(!panel.showing().goes_back(), "now Back closes the panel");
    panel.paste();
    assert!(
        !panel.showing().goes_back(),
        "Back closes a panel opened to paste"
    );
    let pasting = panel.clone();
    panel.back();
    assert_eq!(
        panel, pasting,
        "Back has nothing to go back to, so the first Escape closes it"
    );
}

/// A panel whose page is taller than the room it is given keeps its footer
/// on its own bottom edge and scrolls the page behind it, so no control is
/// lost to the room it was given.
#[test]
fn a_panel_taller_than_its_room_scrolls_rather_than_dropping_a_control() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let mut app =
        HeadlessApp::<QuickSettingsApp>::capture_at(Some("controls"), (360, 420)).unwrap();
    let card = card(&mut app);
    assert!(
        (card.height() - 420.0).abs() < 1.0,
        "the card is the room it is given: {card:?}"
    );
    let power = target(&mut app, "qs-power");
    assert!(has(&mut app, "qs-power"), "the footer is still on the page");
    assert!(
        power.y1 <= card.y1 + 0.5 && power.y1 > card.y1 - 60.0,
        "the footer stands on the card's bottom edge: {power:?} in {card:?}"
    );
    let notifications = target(&mut app, "qs-row-notifications");
    assert!(
        notifications.y0 > power.y1,
        "the page scrolls behind the footer rather than pushing it off: \
         {notifications:?} against {power:?}"
    );
}

#[test]
fn a_battery_tick_changes_its_row_and_moves_nothing() {
    use tilo_power_mgmt::BatteryTime;
    let (mut app, host) = hosted("controls");
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let before = places(&mut app);
    let words = row_words(&mut app, "qs-row-battery");
    host.update(|state| {
        state.status.battery_time = Some(BatteryTime::ToEmpty(199));
        if let Some(battery) = state.status.battery.as_mut() {
            battery.percent = 75;
        }
    });
    app.settle();
    assert_ne!(
        row_words(&mut app, "qs-row-battery"),
        words,
        "the row says the charge and the minutes the tick brought"
    );
    assert_eq!(
        places(&mut app),
        before,
        "a tick changes one row's words in place: nothing on the page moves"
    );
}

/// Where every control on the page stands, by name.
fn places(app: &mut HeadlessApp<QuickSettingsApp>) -> Vec<(String, Rect)> {
    let mut places: Vec<(String, Rect)> = app.harness().targets().into_iter().collect();
    places.sort_by(|(left, _), (right, _)| left.cmp(right));
    places
}

/// What the control with `test_id` says: its name, its line under it, and its
/// value.
fn row_words(
    app: &mut HeadlessApp<QuickSettingsApp>,
    test_id: &str,
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    fn find<'a>(
        node: &'a ui::headless::InspectionNode,
        test_id: &str,
    ) -> Option<&'a ui::headless::InspectionNode> {
        if node.test_id.as_deref() == Some(test_id) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, test_id))
    }
    let snapshot = app.harness().inspection();
    let said = find(&snapshot.root, test_id)?.semantics.as_ref()?;
    Some((
        said.name.clone(),
        said.description.clone(),
        said.value.clone(),
    ))
}

/// The facts a line says, in order: a line lays its facts out as separate
/// items, so each is its own word under the line's test id.
fn line_facts(app: &mut HeadlessApp<QuickSettingsApp>, test_id: &str) -> Vec<String> {
    fn find<'a>(
        node: &'a ui::headless::InspectionNode,
        test_id: &str,
    ) -> Option<&'a ui::headless::InspectionNode> {
        if node.test_id.as_deref() == Some(test_id) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, test_id))
    }
    fn names(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
        if let Some(name) = &node.name {
            out.push(name.clone());
        }
        for child in &node.children {
            names(child, out);
        }
    }
    let snapshot = app.harness().inspection();
    let mut out = Vec::new();
    let Some(line) = find(&snapshot.root, test_id) else {
        return out;
    };
    for said in std::iter::once(&line.semantics)
        .chain(line.children.iter().map(|child| &child.semantics))
        .flatten()
    {
        names(said, &mut out);
    }
    out
}

/// An output the sound graph names in a paragraph — the card, its mode and
/// the screen after it — is one line under its slider: the screen and the
/// port it plays through.
#[test]
fn a_screen_output_is_named_by_its_screen_and_port() {
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("controls");
    host.update(|state| {
        if let Some(output) = state.sound_outputs.iter_mut().find(|output| output.current) {
            output.name = "AD106M High Definition Audio Controller Digitales Stereo (HDMI) \
                           [LG ULTRAGEAR+]"
                .into();
        }
    });
    app.settle();
    assert_eq!(
        line_facts(&mut app, "qs-volume-device"),
        ["LG ULTRAGEAR+", "HDMI"],
        "the volume row says which output plays, screen and port apart"
    );
    let caption = target(&mut app, "qs-volume-device");
    assert!(
        caption.height() < 30.0,
        "the output's name is one line, not the card's paragraph: {caption:?}"
    );
}

#[test]
fn every_scenario_renders() {
    for scenario in scenarios::NAMES {
        let mut app = staged(scenario);
        assert!(
            !app.harness().targets().is_empty(),
            "{scenario} shows nothing"
        );
    }
}

#[test]
fn tab_walks_the_controls_from_the_tiles() {
    let mut app = staged("controls");
    let d = app.harness();
    assert!(
        d.focus_order().len() > 10,
        "every control takes the keyboard"
    );
    d.tab(false);
    assert_eq!(
        d.focused_access_node()
            .and_then(|node| node.name)
            .as_deref(),
        Some("Wi-Fi"),
        "the first Tab reaches the first tile"
    );
}

/// Every page stands in the panel's footprint: the card is the window the
/// desk gives a popover, whatever the page shows, so the footer stands on
/// the window's bottom edge and nothing it draws leaves the window.
#[test]
fn every_page_stands_in_the_panels_footprint_or_scrolls_inside_it() {
    for scenario in scenarios::NAMES {
        let mut app =
            HeadlessApp::<QuickSettingsApp>::capture_at(Some(scenario), (380, 760)).unwrap();
        let window = card(&mut app);
        assert!(
            (window.height() - 760.0).abs() < 1.0 && (window.width() - 380.0).abs() < 1.0,
            "{scenario} fills the panel's window: {window:?}"
        );
        let footer = target(&mut app, "qs-power");
        assert!(
            footer.y1 <= window.y1 + 0.5 && footer.y1 > window.y1 - 40.0,
            "{scenario} keeps its footer on the window's bottom edge: {footer:?} in {window:?}"
        );
        for control in ["qs-lock", "qs-settings", "qs-account"] {
            let rect = target(&mut app, control);
            assert!(
                rect.x0 >= window.x0 - 0.5
                    && rect.x1 <= window.x1 + 0.5
                    && rect.y1 <= window.y1 + 0.5,
                "{scenario} draws {control} inside the window: {rect:?} in {window:?}"
            );
        }
    }
}

/// A page change never moves the panel: the footer keeps its edge and the
/// page under it starts where the page before it did, whatever the two hold.
#[test]
fn a_page_change_keeps_the_panels_edges() {
    let mut app = staged("controls");
    let footer = target(&mut app, "qs-power");
    let wifi = target(&mut app, "qs-tile-wifi");

    // An accessory's page, which stands one deep over the controls.
    app.invoke(QuickSettings::SHOW_ACCESSORY.with("00:1B:66:A1:B2:C3".to_owned()))
        .unwrap();
    app.settle();
    assert!(
        !has(&mut app, "qs-tile-wifi"),
        "the page replaced the controls"
    );
    assert_eq!(
        target(&mut app, "qs-power"),
        footer,
        "the accessory's page leaves the footer where the controls had it"
    );

    // Back to the controls, and a section under them.
    app.invoke(QuickSettings::BACK.invoke()).unwrap();
    app.settle();
    open_details(&mut app, "qs-tile-wifi");
    assert!(has(&mut app, "qs-section-wifi"), "the section shows");
    assert_eq!(
        target(&mut app, "qs-power"),
        footer,
        "a section stands in the same card as the page it came from"
    );
    let section_first = target(&mut app, "qs-section-wifi");
    assert!(
        section_first.y0 >= wifi.y0,
        "and its controls start where the page under it did: {section_first:?} against {wifi:?}"
    );
}
