//! What an agent's task run did, marked where the person sees it (task-legibility.md §3), in
//! the Notes window as a person uses it: the run goes through the API as the broker sends it,
//! stamped with its run; the rows it made carry the agent's face and colour at their own
//! height, the window's header counts them, the field it renamed takes its colour, all drawn
//! in pixels; and pressing a mark asks the shell to show the run.
//!
//! With `TILO_LEGIBILITY_SHOTS=<dir>` the tests write the window, light and dark, with the
//! marks.

use super::Session;
use crate::change_marks::Driven as _;
use serde_json::Map;

use crate::change_marks::{
    action_id, chroma_in, marks, named, published_with, rows, run, settle, shots_dir, text_field,
};

const NOTES: &str = "so.tilo.notes";

/// The Notes window with one note of Ada's and two the agent's run made, and the notebook.
fn notes_after_a_run() -> (Session, String) {
    let mut app = Session::new("so.tilo.notes", "ada");
    settle(&mut app);
    let root = super::root_id(&app, NOTES);
    let notebook = format!("tl:{NOTES}:{root}");
    let new_note = action_id(&mut app, &notebook, "New note");
    // Ada's own note: nothing to mark.
    app.invoke(None, None, &notebook, &new_note, &Map::new())
        .unwrap();
    settle(&mut app);
    assert!(
        marks(&mut app).is_empty(),
        "the person's own note is not marked"
    );
    let (agent, ambient) = run();
    for _ in 0..2 {
        app.invoke(
            Some(agent.clone()),
            Some(&ambient),
            &notebook,
            &new_note,
            &Map::new(),
        )
        .unwrap();
    }
    settle(&mut app);
    (app, notebook)
}

fn shoot(app: &mut Session, name: &str) {
    let Some(dir) = shots_dir() else {
        return;
    };
    for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
        app.harness().set_theme(theme);
        settle(app);
        app.harness()
            .render_png(&dir.join(format!("{name}-{shade}.png")));
    }
    app.harness().set_theme(ui::Theme::light());
    settle(app);
}

#[test]
fn the_window_shows_what_the_run_did_in_its_colour_and_opens_the_run() {
    let (mut app, _) = notes_after_a_run();
    let ground = chroma_in(&mut app, [0.0, 0.0, 8.0, 8.0]);
    // The notebook lists each note once: the person's own and the two the run made.
    let listed = rows(&mut app);
    assert_eq!(listed.len(), 3, "{listed:?}");
    // The window's header says how many changed, and by whom, in the agent's colour.
    let header = marks(&mut app);
    assert_eq!(header.len(), 1, "{header:?}");
    assert!(
        chroma_in(&mut app, header[0].1) > ground.saturating_add(24),
        "the header's mark is in the agent's colour"
    );
    shoot(&mut app, "notes-marks");

    // A note the run made says so under the window bar's History: its history names the run, in
    // the agent's colour, under the person's own entry. The second of the rows is one of the
    // run's: the person's own note stands first.
    let made = listed[1].1;
    app.harness()
        .click_at(made[0] + made[2] / 4.0, made[1] + made[3] / 2.0);
    settle(&mut app);
    assert!(
        crate::history_window::press_history(&mut app),
        "the window's History opens"
    );
    let face = named(&mut app, "Avatar", "Helper");
    assert!(
        chroma_in(&mut app, face) > ground.saturating_add(24),
        "the run's entry stands in the agent's colour"
    );
    named(&mut app, "Text", "Tidy the notes");
}

#[test]
fn the_field_the_run_renamed_takes_its_colour() {
    let (mut app, notebook) = notes_after_a_run();
    let ground = chroma_in(&mut app, [0.0, 0.0, 8.0, 8.0]);
    let (agent, ambient) = run();
    let item = published_with(&mut app, &notebook, "Rename…");
    let retitle = action_id(&mut app, &item, "Rename…");
    let mut title = Map::new();
    title.insert("title".to_owned(), "Meeting notes".into());
    app.invoke(Some(agent), Some(&ambient), &item, &retitle, &title)
        .unwrap();
    settle(&mut app);
    let row = named(&mut app, "ListRow", "Meeting notes");
    app.harness()
        .click_at(row[0] + row[2] / 4.0, row[1] + row[3] / 2.0);
    settle(&mut app);
    let field = text_field(&mut app, "Meeting notes");
    // Inside the words it wrote, clear of the caret that ends them.
    let end = [field[0] + 12.0, field[1] + 2.0, 96.0, field[3] - 4.0];
    assert!(
        chroma_in(&mut app, end) > ground.saturating_add(8),
        "the field the run changed is tinted in its colour"
    );
    shoot(&mut app, "note-field");
}
