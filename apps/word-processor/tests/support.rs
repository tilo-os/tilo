use std::path::Path;

use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;
use word_processor::actions::RichTextAction;
use word_processor::framework::WordProcessorApp;

pub type App = TestApp<WordProcessorApp>;

pub fn open() -> App {
    ui::i18n::I18nManager::init("en-US");
    App::new().unwrap()
}

pub fn launched(path: &Path) -> App {
    ui::i18n::I18nManager::init("en-US");
    App::launched(&[path.to_str().unwrap()]).unwrap()
}

pub fn apply(app: &mut App, action: &RichTextAction) {
    app.invoke(Invoke::edit(&word_processor::model::edit(action)).unwrap())
        .unwrap();
}

pub fn snapshot(app: &App) -> tilo_doc::DocSnapshot {
    app.read_document(|doc| {
        let containers = word_processor::model::open(doc).unwrap();
        tilo_doc::model::materialize(doc, &containers)
    })
}

pub fn texts(app: &App) -> Vec<String> {
    snapshot(app)
        .blocks
        .into_iter()
        .map(|block| block.text)
        .collect()
}

pub fn last_block(app: &App) -> String {
    snapshot(app).blocks.last().unwrap().id.clone()
}

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
