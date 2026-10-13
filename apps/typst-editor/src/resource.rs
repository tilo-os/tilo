//! Typst Editor's record: a workspace of Typst files, each with its name,
//! the path it is saved at and its source.
//!
//! Renaming a file and typing in its source are the built-in field edits
//! every path offers (`Workspace::paths().files.item(&id).source.splice(..)`);
//! the methods below are the operations that are more than setting a field.
//! Compile, open, save, export and zoom are effects and views of the app
//! ([`crate::actions::TypstEffect`]); live diagnostics and preview pages are
//! rows the surface publishes.

use std::rc::Rc;

use ui::framework::{
    Fixture, LaunchInput, LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::record::RecordId;
use ui::tilo_resource::{self, ActionCx, Atom, Collection, Id, Invoke, Model, Refusal, resource};
use ui::widget::Element;

use crate::app::TypstApp;
use crate::samples::{DEFAULT_INDEX, SAMPLES};

ui::embedded_catalog!(static CATALOG, ["en-US"]);

/// One Typst file of the workspace.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct TypstFile {
    /// Name
    ///
    /// The file name shown in the switcher (`report.typ`). Written whole, so
    /// two people renaming it at once do not merge two names.
    #[model(label)]
    pub name: Atom,
    /// Path
    ///
    /// Where the file was opened from or last saved to, if anywhere. Only
    /// opening and saving change it.
    #[model(readonly)]
    pub path: Option<Atom>,
    /// Source
    ///
    /// The Typst text, merged character by character.
    pub source: String,
}

/// The workspace: its files, in the order they were added.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    pub files: Collection<TypstFile>,
}

impl Workspace {
    /// The file `id`, if the workspace has it.
    pub fn file(&self, id: &Id) -> Option<&TypstFile> {
        self.files.get(id)
    }
}

#[resource(kind = "typst-editor.workspace", label = "Typst workspace")]
impl Workspace {
    /// Create a Typst file.
    #[action]
    pub fn new_file(
        &mut self,
        cx: &mut ActionCx,
        #[param("File name")] name: String,
        #[param("Where it is saved, if anywhere")] path: Option<String>,
        #[param("Its Typst source; empty when none")] source: Option<String>,
    ) {
        self.files.push(
            cx.id(),
            TypstFile {
                name: Atom(name.trim().to_owned()),
                path: path.map(Atom),
                source: source.unwrap_or_default(),
            },
        );
    }

    /// Record where a file was saved.
    ///
    /// It lives at that path now and takes the path's name.
    #[action]
    pub fn saved_at(
        &mut self,
        file: &Id,
        #[param("Where it was saved")] path: String,
    ) -> Result<(), Refusal> {
        let file = self.files.get_mut(file).ok_or("no such file")?;
        file.name = Atom(file_name(&path));
        file.path = Some(Atom(path));
        Ok(())
    }

    /// Load a file from disk into this one.
    ///
    /// Its source becomes what the file holds, and it lives at the file's
    /// path.
    #[action]
    pub fn load(
        &mut self,
        file: &Id,
        #[param("Where the file was read from")] path: String,
        #[param("What it holds")] source: String,
    ) -> Result<(), Refusal> {
        let file = self.files.get_mut(file).ok_or("no such file")?;
        file.name = Atom(file_name(&path));
        file.path = Some(Atom(path));
        file.source = source;
        Ok(())
    }

    /// Close a file.
    ///
    /// It leaves the workspace.
    #[action(destructive)]
    pub fn close_file(&mut self, file: &Id) -> Result<(), Refusal> {
        self.files
            .remove(file)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such file"))
    }
}

/// The last component of `path`, which names a file saved there.
pub(crate) fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("untitled.typ")
        .to_owned()
}

/// The action that adds a new file named `name` with `source`, saved at
/// `path` if anywhere.
pub(crate) fn new_file(name: &str, path: Option<&str>, source: &str) -> Invoke<Workspace> {
    Workspace::NEW_FILE.with(workspace::NewFile {
        name: name.to_owned(),
        path: path.map(str::to_owned),
        source: Some(source.to_owned()),
    })
}

/// The bundled example a capture shows, with a fixed id so every capture of
/// a scenario is the same.
pub(crate) fn sample_files(scenario: Option<&str>) -> Vec<(Id, TypstFile)> {
    SAMPLES
        .iter()
        .enumerate()
        .filter(|(index, _)| *index == DEFAULT_INDEX)
        .map(|(index, sample)| {
            let source = match scenario {
                Some("scrolled") if index == DEFAULT_INDEX => crate::app::SCROLL_DEMO,
                Some("errors") if index == DEFAULT_INDEX => crate::samples::ERROR_WARNING,
                _ => sample.source,
            };
            let file = TypstFile {
                name: Atom(sample.name.to_owned()),
                path: None,
                source: source.to_owned(),
            };
            (sample_id(index), file)
        })
        .collect()
}

/// The id of the bundled example at `index` in a capture.
pub(crate) fn sample_id(index: usize) -> Id {
    Id::from_raw(format!("sample-{index}"))
}

/// Framework app: the typst widget is the surface over [`Workspace`].
pub struct TypstEditorShell;

impl ResourceApp for TypstEditorShell {
    type Resource = Workspace;
    const ID: &'static str = "so.tilo.typst-editor";
    const TITLE: &'static str = "Typst";
    const ICON: Option<&'static str> = Some("typst-editor");
    /// The narrow-window scenarios are captured at a phone width.
    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        scenario
            .and_then(|name| matches!(name, "preview" | "documents").then_some((520, 820)))
            .unwrap_or((1280, 800))
    }
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        cx.surface(TypstApp::new())
    }

    /// A `.typ` path on the command line opens that file's own workspace.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        input.argument().map(RecordId::identify_file)
    }

    /// A window edits one document: the file it was opened on, or a new
    /// empty one. Another document is another editor in a desktop tab.
    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<Workspace>> {
        if !created {
            return Vec::new();
        }
        let file = if let Some(path) = input.argument() {
            new_file(
                &file_name(path),
                Some(path),
                &crate::app::read_text(std::path::Path::new(path)).unwrap_or_default(),
            )
        } else if let Some(file) = input.file() {
            new_file(file.name(), None, &String::from_utf8_lossy(file.bytes()))
        } else {
            new_file("untitled.typ", None, "")
        };
        vec![file]
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "preview",
            "documents",
            "pages",
            "problems",
            "long-line",
            "errors",
            "editor-select",
            "preview-zoom",
            "preview-pan",
            "select",
            "stale",
            "status",
        ]
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn fixture(fixture: &mut Fixture<Workspace>) {
        fixture.provide(CaptureFile(sample_id(DEFAULT_INDEX)));
    }

    /// Every capture shows the bundled examples.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Workspace>> {
        sample_files(name)
            .iter()
            .enumerate()
            .filter_map(|(at, (id, file))| {
                Invoke::edit(&Workspace::paths().files.insert_as(id, at, file)).ok()
            })
            .collect()
    }
}

/// The file a capture shows first.
pub(crate) struct CaptureFile(pub(crate) Id);

impl ResourceSurface<Workspace, crate::actions::TypstEffect> for TypstApp {
    fn effect(
        &mut self,
        effect: crate::actions::TypstEffect,
        cx: &mut ui::framework::EffectCx<Workspace, Self>,
    ) -> ui::framework::EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn present(&mut self, state: &Rc<Workspace>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Workspace>) {
        self.attach_surface(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.live_rows()
            .into_iter()
            .flat_map(|row| {
                let resource = row.into_resource();
                std::iter::once(resource.clone()).chain(resource.children)
            })
            .find(|resource| resource.id.as_str() == id)
            .map(ui::framework::LiveRow::from_resource)
    }
}

#[cfg(test)]
mod tests;
