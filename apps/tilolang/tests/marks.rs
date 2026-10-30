//! What an agent's task run did, marked where the person sees it (task-legibility.md §3), in
//! the Notes window as a person uses it: the run goes through the door as the broker sends it,
//! stamped with its run; the rows it made and the window's header carry a mark in the agent's
//! colour, drawn in pixels; and activating a mark asks the shell to show the run.
//!
//! With `TILO_LEGIBILITY_SHOTS=<dir>` the test writes the window, light and dark, with the
//! marks.

use serde_json::Map;
use tilolang_tilo::action::resource_ids;
use ui::framework::testing::HostedApp;

use crate::change_marks::{
    RUN, action_id, chroma_in, marks, named, published_with, run, settle, shots_dir, text_field,
};

const NOTES: &str = "so.tilo.lang.notes";

#[test]
fn the_rows_and_the_window_show_what_the_run_did_in_its_colour_and_open_the_run() {
    let mut app = HostedApp::open(crate::support::installed("notes"), "ada").unwrap();
    settle(&mut app);
    let root = app.read(|doc| resource_ids(doc, NOTES)).remove(0);
    let notebook = format!("tl:{NOTES}:{root}");
    let new_note = action_id(&mut app, &notebook, "New note");
    // Ada's own note: nothing to mark.
    app.invoke(None, None, &notebook, &new_note, &Map::new(), false)
        .unwrap();
    settle(&mut app);
    assert!(
        marks(&mut app).is_empty(),
        "the person's own note is not marked"
    );

    // The agent's run makes two notes.
    let (agent, ambient) = run();
    for _ in 0..2 {
        app.invoke(
            Some(agent.clone()),
            Some(&ambient),
            &notebook,
            &new_note,
            &Map::new(),
            false,
        )
        .unwrap();
    }
    settle(&mut app);
    let shown = marks(&mut app);
    // A mark on each row the run made, and one in the window's header.
    assert!(shown.len() >= 3, "{shown:?}");
    let ground = chroma_in(&mut app, [0.0, 0.0, 8.0, 8.0]);
    for (name, rect) in &shown {
        assert!(
            chroma_in(&mut app, *rect) > ground.saturating_add(24),
            "{name} is drawn in the agent's colour"
        );
    }

    if let Some(dir) = shots_dir() {
        for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
            app.harness().set_theme(theme);
            settle(&mut app);
            app.harness()
                .render_png(&dir.join(format!("notes-marks-{shade}.png")));
        }
    }

    // The run names one of its notes: opened, its title field is in the agent's colour.
    let item = published_with(&mut app, &notebook, "Retitle");
    let retitle = action_id(&mut app, &item, "Retitle");
    let mut title = Map::new();
    title.insert("title".to_owned(), "Meeting notes".into());
    app.invoke(
        Some(agent.clone()),
        Some(&ambient),
        &item,
        &retitle,
        &title,
        false,
    )
    .unwrap();
    settle(&mut app);
    let row = named(&mut app, "SwipeRow", "Meeting notes");
    app.harness()
        .click_at(row[0] + row[2] / 4.0, row[1] + row[3] / 2.0);
    settle(&mut app);
    let field = text_field(&mut app, "Meeting notes");
    // Its far end, clear of the text and the caret.
    let end = [
        field[0] + field[2] - 12.0,
        field[1] + 2.0,
        8.0,
        field[3] - 4.0,
    ];
    assert!(
        chroma_in(&mut app, end) > ground.saturating_add(8),
        "the field the run changed is tinted in its colour"
    );
    if let Some(dir) = shots_dir() {
        for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
            app.harness().set_theme(theme);
            settle(&mut app);
            app.harness()
                .render_png(&dir.join(format!("note-field-{shade}.png")));
        }
        app.harness().set_theme(ui::Theme::light());
        settle(&mut app);
    }

    // Activating a mark asks the shell to show the run that made it.
    let shown = marks(&mut app);
    let (_, rect) = shown[0];
    app.harness()
        .click_at(rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
    settle(&mut app);
    let mut asked = Vec::new();
    while let Some(intent) = app.harness().take_host_intent() {
        asked.push(intent);
    }
    assert!(
        asked.iter().any(|intent| matches!(
            intent,
            ui::widget::AppHostIntent::ShowTaskRun { group } if group == RUN
        )),
        "the mark shows its run: {asked:?}"
    );
}
