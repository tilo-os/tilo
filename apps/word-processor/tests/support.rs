//! Driving the word processor the way a person and its host do: launched on
//! a file, edited through the actions its controls and the palette submit,
//! saved with its own Save As.

use std::path::Path;

use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;
use word_processor::actions::RichTextAction;
use word_processor::framework::WordProcessorApp;

pub type App = TestApp<WordProcessorApp>;

/// The app on a new document.
pub fn open() -> App {
    ui::i18n::I18nManager::init("en-US");
    App::new().unwrap()
}

/// The app launched on `path`, as `word-processor PATH` opens it.
pub fn launched(path: &Path) -> App {
    ui::i18n::I18nManager::init("en-US");
    App::launched(&[path.to_str().unwrap()]).unwrap()
}

/// Submit `action` through the app's door, as a control or the palette does.
pub fn apply(app: &mut App, action: &RichTextAction) {
    app.invoke(Invoke::edit(&word_processor::model::edit(action)).unwrap())
        .unwrap();
}

/// The document as it is now.
pub fn snapshot(app: &App) -> tilo_doc::DocSnapshot {
    app.read_document(|doc| {
        let containers = word_processor::model::open(doc).unwrap();
        tilo_doc::model::materialize(doc, &containers)
    })
}

/// Every paragraph's text, in order.
pub fn texts(app: &App) -> Vec<String> {
    snapshot(app)
        .blocks
        .into_iter()
        .map(|block| block.text)
        .collect()
}

/// The id of the last paragraph.
pub fn last_block(app: &App) -> String {
    snapshot(app).blocks.last().unwrap().id.clone()
}

/// Save the document to `path` with the app's Save As, in the format the
/// path names.
pub fn save_as(app: &mut App, path: &Path) {
    let mut arguments = serde_json::Map::new();
    arguments.insert("path".into(), path.to_string_lossy().into_owned().into());
    app.harness()
        .invoke_action(
            &word_processor::effects::WordProcessorEffect::SAVE_AS.on("word-processor.document"),
            &arguments,
            true,
            None,
        )
        .unwrap();
    assert!(path.exists(), "Save As wrote {}", path.display());
}
