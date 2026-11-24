//! The scratchpad app (`apps/tl/scratchpad/app.tl`) end to end: the one note a workspace keeps,
//! its editor, and the name it takes from its first line.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::rich::RichValue;
    use tilolang_runtime::{AmStore, EditOutcome, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = include_str!("../../../../apps/tl/scratchpad/app.tl");

    /// The note, opened on its page, with the editor a person writes in.
    fn opened() -> (
        App,
        Runtime,
        TestAuthority,
        tilolang_runtime::ResourceRef,
        tilolang_runtime::ViewInstance,
    ) {
        let app = App::compile(SOURCE, &[]);
        let auth = TestAuthority::new();
        let rt = Runtime::new(AmStore::new(), auth.clone()).unwrap();
        auth.trust(&app.load(&rt));
        let n = rt.create_root("Note").unwrap();
        let view = rt
            .open_view(&me(), &app.view("note_page"), vec![Value::resource(&n)])
            .unwrap();
        (app, rt, auth, n, view)
    }

    tilo_test_env::scenario! {
        fn the_scratchpad_is_one_note_its_editor_writes;

        phase a_new_note_opens_empty_and_its_editor_takes_what_is_written;
        {
            let (_app, rt, _auth, n, mut view) = opened();
            assert_eq!(rt.read(&n, "body").unwrap(), Value::Rich(RichValue::default()));
            let editors = of_kind(view.tree(), "ui.BlockEditor");
            assert_eq!(editors.len(), 1, "the page is the note's editor:\n{}", dump(view.tree()));
            let body = editors[0].key.clone();
            assert_eq!(
                view.edit(&body, &Value::Rich(RichValue::from_text("Buy stamps."))).unwrap(),
                EditOutcome::Applied
            );
            view.poll_patches().unwrap();
            let Value::Rich(body) = rt.read(&n, "body").unwrap() else {
                panic!("the note's body is rich text");
            };
            let said: Vec<&str> = body.blocks.iter().map(|block| block.text.as_str()).collect();
            assert_eq!(said, ["Buy stamps."]);
            // The system keeps the write: the note's making and its one edit are two versions.
            assert_eq!(rt.versions(&n).unwrap().len(), 2, "the write is one change of the note");
        }

        phase the_note_is_named_by_the_first_line_it_holds;
        {
            let (_app, rt, _auth, n, mut view) = opened();
            // An unwritten note is named as the app is: the root has no line to take one from.
            assert_eq!(rt.title(&n).unwrap(), "Scratchpad");
            let body = of_kind(view.tree(), "ui.BlockEditor")[0].key.clone();
            view.edit(
                &body,
                &Value::Rich(RichValue::from_text(
                    "Notes for Thursday\n\nPick up the printed tickets before noon.",
                )),
            )
            .unwrap();
            view.poll_patches().unwrap();
            assert_eq!(rt.title(&n).unwrap(), "Notes for Thursday");
        }
    }

    #[test]
    fn the_scenarios_stage_the_sample_note_and_one_long_enough_to_scroll() {
        let app = App::compile(SOURCE, &[]);
        let names: Vec<&str> = app
            .meta()
            .scenarios
            .iter()
            .map(|scenario| scenario.name.as_str())
            .collect();
        assert_eq!(names, ["a note", "a long note"]);
    }
}
