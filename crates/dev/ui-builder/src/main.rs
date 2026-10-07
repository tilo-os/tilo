//! A native visual editor for Tilo serialized UI graphs: `tilo-ui-builder
//! FILE` opens the graph in FILE and saves it back there.
//!
//! The graph is the file, not a document: the app's resource holds nothing,
//! and the editor is the surface over it.

use std::path::{Path, PathBuf};

use ui::framework::{ResourceApp, ResourceSurface, ViewCx};
use ui::tilo_resource::{Doc, ModelError, NoAction, Resource};
use ui::widget::{Decorator, Element, Text};
use ui::{Theme, UiGraphEditor, UiGraphHandle};

/// The editor's resource: the door its window is published on. The graph it
/// edits is the file the launch names.
struct UiGraph;

impl Resource for UiGraph {
    const KIND: &'static str = "ui-builder.graph";
    const LABEL: &'static str = "UI graph";
    const ACTION_TYPE: &'static str = "ui-builder.action";

    type State = ();
    type Action = NoAction;

    fn read(_doc: &Doc) -> Result<(), ModelError> {
        Ok(())
    }
}

struct UiBuilder;

impl ResourceApp for UiBuilder {
    type Resource = UiGraph;
    const ID: &'static str = "so.tilo.ui-builder";
    const TITLE: &'static str = "UI Builder";
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<UiGraph>) -> Element {
        let Some(path) = cx.launch_arguments().first().map(PathBuf::from) else {
            return Box::new(Text::new("tilo-ui-builder requires a UI graph path"));
        };
        match editor(&path) {
            Ok(editor) => cx.surface(GraphSurface(editor)),
            Err(error) => Box::new(Text::new(&error)),
        }
    }
}

/// The editor of the graph in `path`, saving back to it.
fn editor(path: &Path) -> Result<UiGraphEditor, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let handle = UiGraphHandle::from_json(&source)
        .map_err(|error| format!("invalid {}: {error}", path.display()))?;
    let save_path = path.to_path_buf();
    Ok(UiGraphEditor::new(handle, Theme::dark())
        .map_err(|error| format!("build {}: {error}", path.display()))?
        .on_save(move |source| save(&save_path, source)))
}

/// The graph editor as the surface over the app's resource, which holds
/// nothing for it to present.
struct GraphSurface(UiGraphEditor);

impl Decorator for GraphSurface {
    type Inner = UiGraphEditor;

    fn inner(&self) -> &UiGraphEditor {
        &self.0
    }

    fn inner_mut(&mut self) -> &mut UiGraphEditor {
        &mut self.0
    }
}

impl ResourceSurface<UiGraph> for GraphSurface {
    fn present(&mut self, _state: &std::rc::Rc<()>) {}
}

fn save(path: &Path, source: &str) -> Result<(), String> {
    let mut canonical = source.to_owned();
    if !canonical.ends_with('\n') {
        canonical.push('\n');
    }
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = std::fs::metadata(path).map_or(0o644, |metadata| metadata.permissions().mode());
        tilo_files::native::atomic::write_with_mode(path, canonical.as_bytes(), mode)
    };
    #[cfg(not(unix))]
    let result = tilo_files::native::atomic::write(path, canonical.as_bytes());
    result.map_err(|error| format!("write {}: {error}", path.display()))
}

ui::main!(UiBuilder);
