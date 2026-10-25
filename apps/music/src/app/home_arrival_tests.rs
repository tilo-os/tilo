//! Home on its way from a catalogue: its placeholder steps on at
//! deadlines, never by running frames.

use super::*;

#[test]
fn a_home_on_its_way_steps_its_placeholder_on_at_deadlines() {
    let mut app = MusicApp::new();
    app.reload(&crate::seed::empty_document());
    app.use_worker(crate::stream::StreamWorker::spawn_offline());
    app.catalogue_connected = true;
    app.ensure_home();
    let asked = app
        .home_asked
        .expect("Home is asked of the connected catalogue");
    assert_eq!(
        app.arriving_step_at(asked),
        Some(asked + ui::arriving::GRACE),
        "its placeholder steps on at a deadline: waiting draws no frames"
    );
}
