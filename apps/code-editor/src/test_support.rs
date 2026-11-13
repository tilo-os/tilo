//! The editor as tests drive it: launched on the sample project in a folder of
//! its own, through the same door as `code-editor FOLDER`.

use std::path::PathBuf;

use ui::framework::testing::TestApp;

use crate::app::CodeApp;
use crate::effects::CodeEditorEffect;
use crate::resource::CodeEditorShell;

pub(crate) struct Sample {
    _directory: tempfile::TempDir,
    pub(crate) root: PathBuf,
    pub(crate) app: TestApp<CodeEditorShell>,
}

impl Sample {
    pub(crate) fn open(&mut self, relative: &str) {
        let path = crate::disk::canonical(&self.root.join(relative));
        self.app
            .effect(&CodeEditorEffect::OpenFile {
                path: Some(ui::tilo_resource::AbsolutePath::new(path).unwrap()),
            })
            .unwrap();
        self.app.settle();
    }

    pub(crate) fn with_doc<T: 'static>(
        &mut self,
        visit: impl FnOnce(&mut CodeApp, &mut ui::widget::Edits<'_>) -> T + 'static,
    ) -> T {
        with_doc(&mut self.app, visit)
    }

    pub(crate) fn editor<T>(&mut self, read: impl FnOnce(&mut CodeApp) -> T) -> T {
        editor(&mut self.app, read)
    }
}

pub(crate) fn sample() -> Sample {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join(crate::sample_project::NAME);
    crate::sample_project::write(&root).unwrap();
    let app = TestApp::<CodeEditorShell>::launched(&[root.to_str().unwrap()]).unwrap();
    app.settings::<crate::settings::CodeSettings>()
        .set(&crate::settings::CodeSettings {
            language_servers: false,
            ..Default::default()
        })
        .unwrap();
    let mut app = app;
    app.settle();
    Sample {
        _directory: directory,
        root,
        app,
    }
}

pub(crate) fn editor<T>(
    app: &mut TestApp<CodeEditorShell>,
    read: impl FnOnce(&mut CodeApp) -> T,
) -> T {
    app.with_surface::<CodeApp, _>(read)
        .expect("the editor is mounted")
}

pub(crate) fn with_doc<T: 'static>(
    app: &mut TestApp<CodeEditorShell>,
    visit: impl FnOnce(&mut CodeApp, &mut ui::widget::Edits<'_>) -> T + 'static,
) -> T {
    let out = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot = std::rc::Rc::clone(&out);
    editor(app, move |app| {
        app.test_hook = Some(Box::new(move |app, doc| {
            *slot.borrow_mut() = Some(visit(app, doc));
        }));
    });
    app.settle();
    let result = out.borrow_mut().take();
    result.expect("the editor settled")
}

pub(crate) fn peer_file(
    doc: &mut automerge::AutoCommit,
    id: &str,
    name: &str,
    language: &str,
    text: &str,
) {
    use ui::tilo_actions::Action as _;
    crate::actions::record_entry_action(id, crate::project::EntryKind::File, name, None, language)
        .apply(doc)
        .unwrap();
    crate::actions::record_text_action(id, text)
        .apply(doc)
        .unwrap();
}
