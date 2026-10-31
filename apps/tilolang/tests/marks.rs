//! What an agent's task run did, marked where the person sees it (task-legibility.md §3), in
//! the Notes window as a person uses it: the run goes through the door as the broker sends it,
//! stamped with its run; the rows it made carry the agent's face and colour at their own
//! height, the window's header counts them, the field it renamed takes its colour, all drawn
//! in pixels; and pressing a mark asks the shell to show the run.
//!
//! With `TILO_LEGIBILITY_SHOTS=<dir>` the tests write the window, light and dark, with the
//! marks.

use serde_json::Map;
use tilolang_tilo::action::resource_ids;
use ui::framework::testing::HostedApp;

use crate::change_marks::{
    RUN, action_id, chroma_in, marks, named, published_with, rows, run, settle, shots_dir,
    text_field,
};

const NOTES: &str = "so.tilo.lang.notes";

/// The Notes window with one note of Ada's and two the agent's run made, and the notebook.
fn notes_after_a_run() -> (HostedApp, String) {
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
    (app, notebook)
}

fn shoot(app: &mut HostedApp, name: &str) {
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
fn the_rows_and_the_window_show_what_the_run_did_in_its_colour_and_open_the_run() {
    let (mut app, _) = notes_after_a_run();
    let ground = chroma_in(&mut app, [0.0, 0.0, 8.0, 8.0]);
    let listed = rows(&mut app);
    // Each row the run made is marked, and keeps its height: the mark is inside the row.
    let marked: Vec<_> = listed.iter().filter(|(_, _, marked)| *marked).collect();
    let plain: Vec<_> = listed.iter().filter(|(_, _, marked)| !*marked).collect();
    assert_eq!((marked.len(), plain.len()), (2, 1), "{listed:?}");
    let height = plain[0].1[3];
    for (name, rect, _) in &marked {
        assert!(
            (rect[3] - height).abs() < 0.5,
            "{name} keeps the row's height"
        );
        // The agent's face on the trailing edge, in its colour.
        let face = [rect[0] + rect[2] - 24.0, rect[1], 24.0, rect[3]];
        assert!(
            chroma_in(&mut app, face) > ground.saturating_add(24),
            "{name} shows the agent's face"
        );
    }
    // And its colour, faintly, under the row; the person's own row is the window's ground.
    let middle = |rect: [f64; 4]| [rect[0] + rect[2] * 0.6, rect[1] + 2.0, 12.0, 4.0];
    assert!(
        chroma_in(&mut app, middle(marked[0].1)) > chroma_in(&mut app, middle(plain[0].1)),
        "the marked row is tinted"
    );
    // The window's header says how many, and by whom.
    let header = marks(&mut app);
    assert_eq!(header.len(), 1, "{header:?}");
    assert!(
        chroma_in(&mut app, header[0].1) > ground.saturating_add(24),
        "the header's mark is in the agent's colour"
    );
    shoot(&mut app, "notes-marks");

    // Pressing the agent's face on a row asks the shell to show the run that made it.
    let rect = marked[0].1;
    app.harness()
        .click_at(rect[0] + rect[2] - 9.0, rect[1] + rect[3] / 2.0);
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

#[test]
fn the_field_the_run_renamed_takes_its_colour() {
    let (mut app, notebook) = notes_after_a_run();
    let ground = chroma_in(&mut app, [0.0, 0.0, 8.0, 8.0]);
    let (agent, ambient) = run();
    let item = published_with(&mut app, &notebook, "Retitle");
    let retitle = action_id(&mut app, &item, "Retitle");
    let mut title = Map::new();
    title.insert("title".to_owned(), "Meeting notes".into());
    app.invoke(Some(agent), Some(&ambient), &item, &retitle, &title, false)
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
    shoot(&mut app, "note-field");
}
