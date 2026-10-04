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
