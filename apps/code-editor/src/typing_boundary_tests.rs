use crate::{CodeEditorShell, doc::EditorDoc, settings::CodeSettings};
use ui::framework::testing::TestApp;

#[test]
fn physical_key_publishes_text_history_and_scene_before_idle_followups() {
    let _home = tilo_test_env::TestHome::new().expect("private profile");
    ui::i18n::I18nManager::init("en-US");
    let directory = tempfile::tempdir().expect("input directory");
    let path = directory.path().join("typing.rs");
    let original = "// typing fixture\npub fn example() {}\n";
    std::fs::write(&path, original).expect("source file");
    let at = ui::time::now();
    ui::time::testing::with_monotonic_time(at, || {
        let mut app = TestApp::<CodeEditorShell>::launched(&[path.to_str().unwrap()])
            .expect("real editor launch");
        app.settings::<CodeSettings>()
            .set(&CodeSettings {
                language_servers: false,
                ..Default::default()
            })
            .expect("editor settings");
        app.harness().resize((1280, 800)).expect("viewport");
        app.settle();
        app.harness()
            .perform_named("editor_view", &ui::a11y::Action::Focus)
            .expect("focus editor");
        app.harness().key("End");
        app.settle();
        let file = app
            .state()
            .files
            .iter()
            .find(|(_, file)| file.name == "typing.rs")
            .expect("source resource")
            .0
            .to_string();
        let before = app.harness().frame_output().scene.clone();
        let history = app.history().len();
        app.harness().key("x");
        assert!(
            !app.harness()
                .queued_work_sources()
                .contains(&"input frame pending")
        );
        assert_eq!(
            app.read_document(|doc| EditorDoc::read_buffer_key(doc, &file)),
            Some("// typing fixturex\npub fn example() {}\n".to_owned())
        );
        assert_eq!(app.history().len(), history + 1);
        assert!(!before.encoding_eq(app.harness().frame_output().scene));
        assert!(app.undo());
        assert_eq!(
            app.read_document(|doc| EditorDoc::read_buffer_key(doc, &file)),
            Some(original.to_owned())
        );
        assert!(app.redo());
        assert_eq!(
            app.read_document(|doc| EditorDoc::read_buffer_key(doc, &file)),
            Some("// typing fixturex\npub fn example() {}\n".to_owned())
        );
    });
}
