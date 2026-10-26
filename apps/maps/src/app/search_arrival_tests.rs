//! A committed search's places on their way from the geocoder.

use super::*;

/// A committed search's places from the geocoder wait in rows of a
/// result's kind, which step on at their deadlines with no frames between
/// and go when the geocoder answers that search.
#[test]
fn a_committed_searchs_places_wait_in_rows_until_the_geocoder_answers() {
    let mut app = MapsApp::offline();
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    let asked = ui::time::now();
    app.search_asked = Some(("bakery".into(), asked));
    let clock = app.search_clock().expect("the places are on their way");
    assert_eq!(clock.source(), &ui::arriving::Source::host(GEOCODER));
    assert_eq!(
        Widget::next_wake_deadline(&app),
        clock.next_step_at(ui::time::now()),
        "the placeholder steps on at its deadline"
    );
    app.net_tx
        .send(NetMsg::Search {
            query: "another".into(),
            results: Ok(Vec::new()),
        })
        .expect("the channel is open");
    app.drain_net();
    assert!(
        app.search_asked.is_some(),
        "another search's answer is not this one's"
    );
    app.net_tx
        .send(NetMsg::Search {
            query: "bakery".into(),
            results: Ok(Vec::new()),
        })
        .expect("the channel is open");
    app.drain_net();
    assert!(app.search_asked.is_none(), "the answer ends the wait");
    assert!(
        app.model.status.contains("No results"),
        "{}",
        app.model.status
    );
}
