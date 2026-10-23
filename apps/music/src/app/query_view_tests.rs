//! Music's search as a view of its own: the same songs as in the library,
//! the stack that returns to the library as it was left, dragging results
//! onto a playlist, chips, the controls group and kept views.

use super::*;
use ui::widget::Queryable as _;

/// The sample library, with the hermetic catalogue a capture uses.
fn searching_app() -> (MusicApp, AutoCommit) {
    let (mut app, doc) = app_with_doc();
    app.disconnect();
    app.search = render::SearchView::hermetic();
    (app, doc)
}

/// Type `text` into the window bar's field, as the bar carries it.
fn type_query(app: &mut MusicApp, doc: &mut AutoCommit, text: &str) {
    app.query_opened();
    app.set_query_text(text);
    app.settle_query();
    draw_once(app, doc);
    draw_once(app, doc);
}

fn ctrl() -> winit::event::Modifiers {
    winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL)
}

/// Ctrl-click the row of song `id`: it joins the selection.
fn ctrl_click(app: &mut MusicApp, doc: &mut AutoCommit, id: &str) {
    let row = track_row(app, id);
    let at = cursor_at(Point::new(row.x0 + row.width() * 0.3, row.center().y));
    send_with(app, &at, ctrl(), doc);
    send_with(app, &press(MouseButton::Left), ctrl(), doc);
    send_with(app, &released(MouseButton::Left), ctrl(), doc);
}

fn selected_songs(app: &MusicApp) -> Vec<String> {
    app.track_tables
        .first()
        .map(|slot| slot.table.selected_ids())
        .unwrap_or_default()
}

#[test]
fn liking_a_song_in_search_shows_it_liked_in_the_library() {
    let (mut app, mut doc) = searching_app();
    app.view = View::Songs;
    draw_once(&mut app, &mut doc);
    type_query(&mut app, &mut doc, "glasslight");
    assert_eq!(
        app.view,
        View::Search,
        "typing turned the view into a search"
    );
    let song = app
        .track_tables
        .first()
        .and_then(|slot| slot.table.rows().iter().find(|row| !row.liked).cloned())
        .map(|row| row.id)
        .expect("a song of the results not liked yet");
    let like = track_control(&mut app, &mut doc, &song, "like", draw_once);
    hosted_click(&mut app, &mut doc, like.center());
    draw_once(&mut app, &mut doc);
    assert!(app.lib.is_liked(&song), "liked through the row's control");

    app.query_closed();
    draw_once(&mut app, &mut doc);
    assert_eq!(app.view, View::Songs);
    let row = app
        .track_tables
        .first()
        .and_then(|slot| {
            slot.table
                .rows()
                .iter()
                .find(|row| row.id == "tk_g1")
                .cloned()
        })
        .expect("the song's row in the library");
    assert!(row.liked, "the library shows the same song liked");
}

#[test]
fn liking_a_catalogue_song_in_search_brings_it_into_the_library_in_place() {
    let (mut app, mut doc) = searching_app();
    app.view = View::Songs;
    draw_once(&mut app, &mut doc);
    type_query(&mut app, &mut doc, "northern");
    let id = live_track_id("smpl-north");
    let before = app.search.engine.results().order();
    assert!(
        before.iter().any(|(_, row)| *row == id),
        "the catalogue's song is a result: {before:?}"
    );
    let like = track_control(&mut app, &mut doc, &id, "like", draw_once);
    hosted_click(&mut app, &mut doc, like.center());
    draw_once(&mut app, &mut doc);
    assert!(
        app.stored_library.tracks.contains_key(&id),
        "it joined the library"
    );
    assert!(app.lib.is_liked(&id));
    assert_eq!(app.search.engine.results().order(), before, "no row moved");
    app.query_closed();
    app.dispatch(
        Action::Nav(View::Liked),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    draw_once(&mut app, &mut doc);
    assert!(app.lib.liked_ids().contains(&id), "Liked lists it");
}

#[test]
fn escape_returns_to_the_library_scrolled_and_selected_as_it_was() {
    let (mut app, mut doc) = searching_app();
    app.view = View::Songs;
    draw_once(&mut app, &mut doc);
    ctrl_click(&mut app, &mut doc, "tk_g2");
    ctrl_click(&mut app, &mut doc, "tk_g3");
    let selected = selected_songs(&app);
    assert_eq!(selected, ["tk_g2", "tk_g3"]);
    app.set_scroll_y(240.0);
    draw_once(&mut app, &mut doc);
    let scrolled = app.scroll_y();
    assert!(scrolled > 0.0, "the library scrolled");

    type_query(&mut app, &mut doc, "tide");
    assert_eq!(app.view, View::Search);
    app.query_closed();
    draw_once(&mut app, &mut doc);
    draw_once(&mut app, &mut doc);
    assert_eq!(app.view, View::Songs);
    assert!(
        (app.scroll_y() - scrolled).abs() < 0.5,
        "scrolled back to {scrolled}, at {}",
        app.scroll_y()
    );
    assert_eq!(selected_songs(&app), selected, "the same songs selected");
}

#[test]
fn dragging_selected_results_onto_a_playlist_adds_them_all() {
    let (mut app, mut doc) = searching_app();
    app.view = View::Songs;
    draw_once(&mut app, &mut doc);
    type_query(&mut app, &mut doc, "light");
    let playlist = app
        .lib
        .playlists()
        .next()
        .map(|list| list.id.clone())
        .expect("a sample playlist");
    let songs: Vec<String> = app
        .track_tables
        .first()
        .map(|slot| {
            slot.table
                .rows()
                .iter()
                .take(2)
                .map(|row| row.id.clone())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(songs.len(), 2, "two songs match");
    for song in &songs {
        ctrl_click(&mut app, &mut doc, song);
    }
    let row = track_row(&app, &songs[0]);
    let start = Point::new(row.x0 + row.width() * 0.3, row.center().y);
    send(&mut app, &cursor_at(start), &mut doc);
    send(&mut app, &press(MouseButton::Left), &mut doc);
    send(
        &mut app,
        &cursor_at(Point::new(start.x, start.y + 40.0)),
        &mut doc,
    );
    assert_eq!(
        app.search.drag.as_ref().map(|drag| drag.tracks.clone()),
        Some(songs.clone()),
        "the selected songs go with the drag"
    );
    draw_once(&mut app, &mut doc);
    // The shell's sidebar, painted from the places Music declares.
    let sidebar = ui::widget::NavShell::new()
        .sidebar(Box::new(
            ui::widget::NavList::new(Vec::new()).fed_by(&app.places),
        ))
        .content(Box::new(ui::widget::Spacer::new()));
    let size = (1280, 800);
    let mut shell =
        ui::headless::FrameHarness::for_root(ui::State::new(), sidebar, size, Theme::dark());
    shell.relayout_at(size).expect("the shell lays out");
    let target = app
        .places
        .shown()
        .and_then(|_| {
            shell
                .root::<ui::widget::NavShell>()
                .sidebar_ref::<ui::widget::NavList>()?
                .row_rect(&shell_place(&View::Playlist(playlist.clone()))?)
        })
        .map(|row| row.center())
        .expect("the playlist in the sidebar");
    send(&mut app, &cursor_at(target), &mut doc);
    send(&mut app, &released(MouseButton::Left), &mut doc);
    let list = app.lib.playlist(&playlist).expect("the playlist");
    for song in &songs {
        assert!(
            list.tracks
                .contains(&ui::tilo_resource::Id::from_raw(song.as_str())),
            "{song} joined the playlist"
        );
    }
    assert!(app.search.drag.is_none());
}

#[test]
fn the_chips_are_artist_year_liked_and_in_library() {
    let (mut app, mut doc) = searching_app();
    app.view = View::Songs;
    type_query(&mut app, &mut doc, "light");
    let results = app.search.engine.results().clone();
    let offers =
        ui::framework::views::facet_offers(app.search.engine.kinds(), &results.query, &results);
    let labels: Vec<&str> = offers.iter().map(|offer| offer.label.as_str()).collect();
    assert_eq!(labels, ["Artist", "Year", "Liked", "In library"]);
    assert!(
        !offers[0].values.is_empty(),
        "the artists the results share"
    );
    // Choosing from the list narrows and the chip reads the choice.
    let artist = offers[0].values[0].value.clone();
    app.dispatch(
        Action::SetQuery(offers[0].toggled(&results.query, &artist)),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    let results = app.search.engine.results().clone();
    let offers =
        ui::framework::views::facet_offers(app.search.engine.kinds(), &results.query, &results);
    assert_eq!(offers[0].label, format!("Artist: {artist}"));
    // A typed token is the same chip.
    app.set_query_text("light liked:yes");
    let query = app.search.engine.results().query.clone();
    assert_eq!(query.text, "light");
    assert!(
        query
            .chips
            .iter()
            .any(|chip| chip.key == "liked" && chip.typed)
    );
}

#[test]
fn in_this_app_shows_the_players_own_controls() {
    let (mut app, mut doc) = searching_app();
    let hits = app.control_hits("shuffle");
    let shuffle = hits.first().expect("the shuffle control matches");
    assert_eq!(
        shuffle.control.kind,
        ui::framework::views::ControlKind::Switch { on: false }
    );
    assert_eq!(shuffle.control.home.as_deref(), Some("player"));
    assert_eq!(shuffle.label(), "Shuffle", "the control's own name");
    assert_eq!(
        app.control_action(&shuffle.spec.id),
        Some(Action::ToggleShuffle),
        "it runs what the player's own button runs"
    );
    app.dispatch(
        Action::ToggleShuffle,
        &mut ui::widget::Edits::lend(&mut doc),
    );
    let hits = app.control_hits("shuffle");
    assert_eq!(
        hits.first().map(|hit| hit.control.kind),
        Some(ui::framework::views::ControlKind::Switch { on: true }),
        "the switch shows the player's state"
    );
}

#[test]
fn a_kept_search_sits_beside_the_designed_views_and_survives_a_restart() {
    use ui::framework::testing::TestApp;
    let mut app = TestApp::<crate::MusicShell>::new().unwrap();
    for invoke in crate::seed::sample_actions() {
        app.invoke(invoke).unwrap();
    }
    app.settle();
    {
        let music = app.surface_mut::<MusicApp>();
        music.query_opened();
        music.set_query_text("light");
        music.on_keep_query();
    }
    app.settle();
    let kept = app.settings::<ui::framework::views::KeptViews>().get();
    assert_eq!(kept.kept_views.len(), 1, "kept as a personal setting");
    let id = kept.kept_views[0].id.clone();
    assert_eq!(kept.kept_views[0].name, "light");

    let mut app = app.reopen().unwrap();
    app.settle();
    let music = app.surface_mut::<MusicApp>();
    assert!(
        music.kept_views().get(&id).is_some(),
        "the kept view is still there after a restart"
    );
    let (places, _) = music.places.shown().expect("the sidebar's places");
    let rows: Vec<&ui::widget::NavItem> =
        places.iter().flat_map(|section| &section.items).collect();
    assert!(
        rows.iter()
            .any(|row| Some(&row.id) == shell_place(&View::Kept(id.clone())).as_ref())
            && rows.iter().any(|row| row.label == "Home"),
        "it sits in the sidebar beside the designed views"
    );
}
