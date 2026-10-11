//! The editor as tests drive it: launched on the sample project in a folder of
//! its own, through the same door as `code-editor FOLDER`.

use std::path::PathBuf;

use ui::framework::testing::TestApp;

use crate::app::CodeApp;
use crate::effects::CodeEditorEffect;
use crate::resource::CodeEditorShell;

/// The sample project written to a folder of its own, and the editor
/// launched on it.
pub(crate) struct Sample {
    _directory: tempfile::TempDir,
    pub(crate) root: PathBuf,
    pub(crate) app: TestApp<CodeEditorShell>,
}

impl Sample {
    /// Open the project file at `relative` as `code-editor FILE` would.
    pub(crate) fn open(&mut self, relative: &str) {
        let path = crate::disk::canonical(&self.root.join(relative));
        self.app
            .effect(&CodeEditorEffect::OpenFile {
                path: Some(path.display().to_string()),
            })
            .unwrap();
        self.app.settle();
    }

    /// Run `visit` on the editor with the live document, as [`with_doc`].
    pub(crate) fn with_doc<T: 'static>(
        &mut self,
        visit: impl FnOnce(&mut CodeApp, &mut ui::widget::Edits<'_>) -> T + 'static,
    ) -> T {
        with_doc(&mut self.app, visit)
    }

    /// Run `read` on the editor.
    pub(crate) fn editor<T>(&mut self, read: impl FnOnce(&mut CodeApp) -> T) -> T {
        editor(&mut self.app, read)
    }
}

/// The sample project, launched.
pub(crate) fn sample() -> Sample {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join(crate::sample_project::NAME);
    crate::sample_project::write(&root).unwrap();
    let app = TestApp::<CodeEditorShell>::launched(&[root.to_str().unwrap()]).unwrap();
    // No file is open yet, so no language server started: tests run none.
    app.settings::<crate::settings::CodeSettings>()
        .set(&crate::settings::CodeSettings {
            language_servers: false,
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

/// Run `read` on the editor `app` mounted.
pub(crate) fn editor<T>(
    app: &mut TestApp<CodeEditorShell>,
    read: impl FnOnce(&mut CodeApp) -> T,
) -> T {
    app.with_surface::<CodeApp, _>(read)
        .expect("the editor is mounted")
}

/// Run `visit` on the editor `app` mounted with the live document, at the
/// start of the next settlement, where the editor edits it: what an event's handler
/// gets. Its edits go through the framework's door like any other.
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

/// A collaborator's file `id` named `name` in `language`, holding `text`,
/// written into `doc` as the project's own actions write one.
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
