//! What a device says of a window it shows is what the tile showing it holds, after every turn.
//!
//! A tile's decoder never holds the loop: the picture of a unit it was handed comes out later,
//! between two turns, and the shell takes it into the tile while it builds a frame. That is after
//! the pass of the shares that turn, so the shares ask their tiles once more when the frame is
//! drawn. Without that, the reading said the window waits for its first picture under a tile that
//! showed it, until something else brought the next pass.

use super::viewers::pictures_heard;
use super::*;

/// Whether `viewer` 's tile of the window shown in `tab` holds the window's picture.
fn holds_picture(viewer: &Member, tab: u64) -> bool {
    viewer
        .session
        .handle()
        .peer_surface_read(tab)
        .is_some_and(|read| read.state == crate::window_share::peer::StreamState::Pictured)
}

/// The device showing a shared window says it is pictured in the turn its tile takes the picture:
/// after every turn, the reading says so exactly when the tile holds one.
///
/// The picture reaches the tile the slow way. The sharing device stands still once the window's
/// first picture is on the viewing device's live session, so nothing pushed after it brings it out of
/// the decoder: it comes out between two turns, and the shell takes it while it builds a frame.
#[test]
fn a_window_is_said_to_be_pictured_in_the_turn_its_tile_takes_the_picture() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut host = member(0x1a, own_shell());
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    share(&mut host, APP, tab);
    let mut viewer = member(0x1b, showing(host.sync.publisher(), APP, tab));
    watch(&mut viewer, tab);

    // Both devices turn until the window's first picture is on the viewing device's live session.
    until(&mut host, &mut window, &mut viewer, |_, viewer, _| {
        pictures_heard(viewer) > 0
    });

    // A window is captured, encoded and decoded here: on a machine busy with the rest of the suite
    // that takes far longer than it does alone.
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut turns = 0_u32;
    loop {
        let (held, said) = (holds_picture(&viewer, tab), says_pictured(&viewer, tab));
        assert_eq!(
            said,
            held,
            "after {turns} turns of the viewing device alone its tile holds the picture ({held}) \
             and its reading says the window is pictured ({said}): {}",
            shares(&viewer.session)
        );
        if held {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the tile never took the picture its live session heard:\n  sharing: {}\n  viewing: {}",
            shares(&host.session),
            shares(&viewer.session)
        );
        // A tile whose decoder gave up on what it was sent asks for a keyframe, and only a sharing
        // device that turns sends one.
        let asked = viewer
            .session
            .handle()
            .peer_surface_read(tab)
            .is_some_and(|read| read.awaiting_keyframe);
        if asked {
            if let Some(client) = window.as_mut() {
                client.exchange();
            }
            host.session.step(Duration::from_millis(2)).expect("step");
            carry(&mut host, &viewer);
        }
        viewer.session.step(Duration::from_millis(2)).expect("step");
        carry(&mut viewer, &host);
        turns += 1;
    }
    let reading = shares(&viewer.session);
    assert_eq!(
        rows(&reading, "received")
            .first()
            .and_then(|stream| stream["waiting_for"].as_str()),
        Some(""),
        "a window its tile shows waits for nothing: {reading}"
    );
}
