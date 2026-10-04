//! Code Editor as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path. Buffer text lives in the shared document as collaborative
//! file records under the project root.
//!
//! Document mutations (create / rename / move / language / delete / replace-text)
//! live on this resource via [`Resource::declarations`] / [`Resource::children`].
//! Opening and saving real files and the editor's commands are the surface's
//! [`CodeEditorEffect`]s; what the surface shows that is not in the document —
//! the files it can open or save, symbols, search results — are its live rows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Fixture, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::record::RecordId;
use ui::tilo_actions::{
    ActionAddress, ActionParam, EncodedAction, PROMPTED_PARAM, ParamType,
    Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, ActionRef, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    CODE_EDITOR_ACTION_TYPE, CodeEditorAction, create_file_action, publish_document_children,
    register,
};
use crate::app::CodeApp;
use crate::doc::{CollaborativeFile, EditorDoc};
use crate::effects::CodeEditorEffect;

/// Stable id prefix for the published project root.
pub const PROJECT_KIND: &str = "code_editor.project";

/// Lightweight project projection for search and the published tree.
#[derive(Clone, Debug, Default)]
pub struct ProjectState {
    pub project_id: Option<String>,
    pub files: Vec<CollaborativeFile>,
    /// Buffer text keyed by file id — base for replace-text Document ops.
    pub buffers: HashMap<String, String>,
}

/// The code-editor project resource.
pub struct CodeEditor;

impl Resource for CodeEditor {
    const KIND: &'static str = PROJECT_KIND;
    const LABEL: &'static str = "Code Editor";
    const ACTION_TYPE: &'static str = CODE_EDITOR_ACTION_TYPE;

    type State = ProjectState;
    type Action = CodeEditorAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<ProjectState, ModelError> {
        let project_id = match doc.get(ROOT, "project_id") {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(id) => Some(id.to_string()),
                _ => None,
            },
            _ => None,
        };
        let files: Vec<_> = EditorDoc::files(doc)
            .into_iter()
            .filter(|file| !file.deleted)
            .collect();
        let mut buffers = HashMap::new();
        for file in files.iter().filter(|file| file.kind == "file") {
            if let Some(text) = EditorDoc::read_buffer_key(doc, &file.id) {
                buffers.insert(file.id.clone(), text);
            }
        }
        Ok(ProjectState {
            project_id,
            files,
            buffers,
        })
    }

    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        // Live editor starts empty — no demo tree and no sample files.
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "new-file",
                "New file",
                vec![ActionParam::optional(
                    PROMPTED_PARAM,
                    ParamType::Text,
                    "File name",
                )],
                |args, _| {
                    let name = args
                        .text(PROMPTED_PARAM)
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or("untitled")
                        .to_string();
                    Ok(create_file_action(
                        uuid::Uuid::new_v4().to_string(),
                        name,
                        None,
                        i64::MAX / 2,
                        String::new(),
                    ))
                },
            )
            .describe("Create a new collaborative file at the project root."),
            ActionDecl::new(
                "new-folder",
                "New folder",
                vec![ActionParam::optional(
                    PROMPTED_PARAM,
                    ParamType::Text,
                    "Folder name",
                )],
                |args, _| {
                    let name = args
                        .text(PROMPTED_PARAM)
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or("untitled")
                        .to_string();
                    Ok(CodeEditorAction::CreateFolder {
                        id: uuid::Uuid::new_v4().to_string(),
                        name,
                        parent: None,
                        order: i64::MAX / 2,
                    })
                },
            )
            .describe("Create a new folder at the project root."),
        ]
    }

    fn children(state: &ProjectState) -> Vec<PublishedResource> {
        publish_document_children(&state.files, &state.buffers)
    }

    fn search(state: &ProjectState) -> Vec<SearchEntry> {
        state
            .files
            .iter()
            .map(|file| {
                let anchor = format!("code_editor.{}:{}", file.kind, file.id);
                SearchEntry {
                    action: (file.kind == "file")
                        .then(|| ActionAddress::new(anchor.clone(), CodeEditorEffect::OPEN.id)),
                    anchor: Some(anchor),
                    title: file.name.clone(),
                    text: file.path.clone(),
                }
            })
            .collect()
    }
}

/// Framework app: the code editor widget is the surface over [`CodeEditor`].
pub struct CodeEditorShell;

/// Every screenshot scenario, over the sample project the fixture writes.
const SCENARIOS: &[&str] = &[
    "terminal",
    "selection",
    "completion",
    "find",
    "find-options",
    "search",
    "conflict",
    "indexing",
    "goto",
    "close-prompt",
    "status",
    "narrow",
    "narrow-sidebar",
];

/// A launch naming a folder opens it as the project; naming a file opens it in
/// a project of its own.
const OPEN_FOLDER: ActionRef<CodeEditor, Option<String>> = ActionRef::new("open-folder", &["path"]);
const OPEN_FILE: ActionRef<CodeEditor, Option<String>> = ActionRef::new("open-file", &["path"]);

/// The project a capture shows: the bundled sample written to a scratch
/// folder, opened the way `code-editor FOLDER` opens one.
pub(crate) struct SampleProject(pub(crate) PathBuf);

/// The absolute form of a launch argument, resolved against the directory the
/// editor was started in, as a shell resolves `code-editor .`.
fn launched_path(argument: &str) -> PathBuf {
    std::path::absolute(argument).unwrap_or_else(|_| PathBuf::from(argument))
}

impl ResourceApp for CodeEditorShell {
    type Resource = CodeEditor;
    const ID: &'static str = "so.tilo.code-editor";
    const TITLE: &'static str = "Code Editor";
    const ICON: Option<&'static str> = Some("code-editor");
    /// The folder or file to open (`code-editor .`, `code-editor src/main.rs`).
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<CodeEditor>) -> Element {
        cx.surface(CodeApp::new())
    }

    /// Every folder and every file opened on its own is a project of its own,
    /// so its files, their unsaved text and its collaborators stay with it.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        let path = launched_path(input.argument()?);
        Some(RecordId::derived(
            "code-editor",
            &RecordId::for_file(crate::disk::canonical(&path)),
        ))
    }

    /// Opening the project reads the folder (or the file) every time it is
    /// launched: where its files are on this machine is not in the document.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<CodeEditor>> {
        let Some(argument) = input.argument() else {
            return Vec::new();
        };
        let path = launched_path(argument);
        let open = if path.is_dir() {
            OPEN_FOLDER
        } else {
            OPEN_FILE
        };
        vec![open.with(Some(path.display().to_string()))]
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            // Below the sidebar breakpoint the tree is an overlay.
            Some("narrow" | "narrow-sidebar") => (720, 800),
            _ => (1280, 800),
        }
    }

    fn fixture(fixture: &mut Fixture<CodeEditor>) {
        match fixture.directory() {
            Ok(directory) => {
                let root = directory.join(crate::sample_project::NAME);
                match crate::sample_project::write(&root) {
                    Ok(()) => fixture.provide(SampleProject(root)),
                    Err(error) => tracing::warn!(%error, "sample project not written"),
                }
            }
            Err(error) => tracing::warn!(%error, "no scratch folder for the sample project"),
        }
    }
}

impl ResourceSurface<CodeEditor, CodeEditorEffect> for CodeApp {
    fn present(&mut self, state: &Rc<ProjectState>) {
        // Adopt shared file records only — never wipe an unpublished local tree.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<CodeEditor>) {
        self.attach_framework(cx.dispatcher(), cx.mode().is_live(), cx.settings());
        if cx.mode() == RunMode::Capture
            && let Some(sample) = cx.fixture::<SampleProject>()
        {
            self.open_sample(&sample.0);
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: CodeEditorEffect,
        cx: &mut EffectCx<CodeEditor, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.live_row(id)
    }
}

/// Framework dispatcher for file records and text edits.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<CodeEditor>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<CodeEditor>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &CodeEditorAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        let _ = self.try_dispatch_with_context(doc, type_id, action, context);
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &CodeEditorAction,
        context: ui::tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => {
                self.last = crate::actions::encode(action).ok();
                dispatcher.apply_now_with(doc, action, context)
            }
            None => self
                .local
                .try_dispatch_with_context(doc, type_id, action, context),
        }
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Hosted commits are recorded by the dispatcher. Standalone tests still
    /// read the local host's rising edge.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::CodeEditorEffect;
    use ui::framework::testing::TestApp;

    use ui::tilo_actions::OperationKind;
    use ui::tilo_resource::Effect as _;

    use crate::test_support::{editor, sample};

    fn file_id(state: &ProjectState, path: &str) -> String {
        state
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("{path} is a file of the project"))
            .id
            .clone()
    }

    fn find<'a>(resources: &'a [PublishedResource], id: &str) -> Option<&'a PublishedResource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }

    fn count(resources: &[PublishedResource], id: &str) -> usize {
        resources
            .iter()
            .map(|resource| usize::from(resource.id.as_str() == id) + count(&resource.children, id))
            .sum()
    }

    #[test]
    fn launching_a_folder_opens_it_as_a_project_of_its_own() {
        let mut sample = sample();
        let root = crate::disk::canonical(&sample.root);
        assert_eq!(
            editor(&mut sample.app, |app| app.project_root()),
            Some(root.clone())
        );
        let state = sample.app.state();
        file_id(&state, "ferris-cli/Cargo.toml");
        assert_eq!(
            sample.app.record(),
            &RecordId::derived("code-editor", &RecordId::for_file(&root)),
            "the folder names its record"
        );

        // A plain launch is the room's own project, not this folder's.
        let plain = TestApp::<CodeEditorShell>::new().unwrap();
        assert_ne!(plain.record(), sample.app.record());

        // Opening it again is the same project: the same file records.
        let records = state.files.len();
        let again = sample
            .app
            .relaunch(&[sample.root.to_str().unwrap()])
            .unwrap();
        assert_eq!(again.state().files.len(), records);
    }

    #[test]
    fn launching_a_file_opens_it_in_a_tab() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("greet.rs");
        std::fs::write(&file, "fn greet() {}\n").unwrap();
        let mut app = TestApp::<CodeEditorShell>::launched(&[file.to_str().unwrap()]).unwrap();
        assert_eq!(
            editor(&mut app, |app| app.active_buffer_name()),
            Some("greet.rs".to_string())
        );
        let state = app.state();
        let id = file_id(
            &state,
            &format!(
                "{}/greet.rs",
                directory.path().file_name().unwrap().to_string_lossy()
            ),
        );
        assert_eq!(
            app.read_document(|doc| EditorDoc::read_buffer_key(doc, &id))
                .as_deref(),
            Some("fn greet() {}\n"),
            "the file's bytes are its buffer"
        );
    }

    #[test]
    fn every_file_is_one_published_resource_with_its_edits_and_the_editors_effects() {
        let mut sample = sample();
        sample.open("src/main.rs");
        let main = file_id(&sample.app.state(), "ferris-cli/src/main.rs");
        let resources = sample.app.harness().resources();
        let root = find(&resources, PROJECT_KIND).expect("the project resource");
        for id in [
            "new-file",
            "new-folder",
            "open-file",
            "open-folder",
            "save",
            "find",
        ] {
            assert!(
                root.actions.iter().any(|action| action.id == id),
                "{id} is on the project"
            );
        }
        let address = crate::actions::file_resource_id(&main);
        assert_eq!(count(&resources, &address), 1, "one resource per file");
        let file = find(&resources, &address).unwrap();
        for id in ["rename", "move", "set-language", "delete", "replace-text"] {
            assert!(
                file.actions
                    .iter()
                    .any(|action| action.id == id && action.kind == OperationKind::Document),
                "{id} edits the file"
            );
        }
        for id in ["open", "save"] {
            assert!(
                file.actions
                    .iter()
                    .any(|action| action.id == id && action.kind == OperationKind::Effect),
                "{id} is the editor's effect on the file"
            );
        }
    }

    #[test]
    fn effects_open_files_and_reveal_symbols_and_search_results() {
        let mut sample = sample();
        // Opening a file reads the folders on its way into the tree.
        sample.open("src/main.rs");
        let prime = file_id(&sample.app.state(), "ferris-cli/src/prime.rs");

        // A file's own row opens it, as a search hit on it does.
        sample
            .app
            .call_on(
                &crate::actions::file_resource_id(&prime),
                "open",
                serde_json::json!({}),
            )
            .unwrap();
        assert_eq!(
            editor(&mut sample.app, |app| app.active_buffer_name()),
            Some("prime.rs".to_string())
        );

        // A symbol the scan found reveals where it is declared.
        sample.app.settle();
        let (address, line) = editor(&mut sample.app, |app| {
            let hit = ui::search::capability::search_providers(app)[0]
                .search("primes_below")
                .into_iter()
                .find(|hit| hit.kind == crate::search_provider::SYMBOL_KIND)
                .expect("the symbol is found");
            let symbol = app
                .search_symbols_for_test()
                .into_iter()
                .find(|symbol| symbol.name == "primes_below")
                .unwrap();
            (hit.action_address().unwrap(), symbol.line)
        });
        sample
            .app
            .call_on(&address.resource, &address.action, serde_json::json!({}))
            .unwrap();
        sample.app.settle();
        assert_eq!(
            editor(&mut sample.app, |app| app.caret_for_test()).0,
            line,
            "the caret lands on the declaration"
        );

        // A hit of the project search shows it beside the results.
        sample
            .app
            .effect(&CodeEditorEffect::Reveal {
                file: None,
                path: Some(
                    crate::disk::canonical(&sample.root.join("src/main.rs"))
                        .display()
                        .to_string(),
                ),
                line: 3,
                column: 4,
                results: true,
            })
            .unwrap();
        sample.app.settle();
        let (name, caret, results) = editor(&mut sample.app, |app| {
            (
                app.active_buffer_name(),
                app.caret_for_test(),
                app.results_shown_for_test(),
            )
        });
        assert_eq!(name.as_deref(), Some("main.rs"));
        assert_eq!(caret, (3, 4));
        assert!(results);
    }

    #[test]
    fn a_missing_path_fails_the_effect_and_says_why() {
        let mut sample = sample();
        let error = sample
            .app
            .effect(&CodeEditorEffect::OpenFile {
                path: Some("/definitely/missing/tilo-code-file".into()),
            })
            .expect_err("a missing file cannot be opened");
        assert!(error.to_string().contains("tilo-code-file"), "{error}");
        assert!(
            editor(&mut sample.app, |app| app.notice().cloned())
                .is_some_and(|notice| notice.is_error)
        );
    }

    #[test]
    fn saving_through_the_files_effect_writes_it_to_disk() {
        let mut sample = sample();
        let state = sample.app.state();
        let readme = file_id(&state, "ferris-cli/README.md");
        sample
            .app
            .effect(&CodeEditorEffect::Open {
                file: readme.clone(),
            })
            .unwrap();
        sample.app.settle();
        let text = sample
            .app
            .read_document(|doc| EditorDoc::read_buffer_key(doc, &readme))
            .unwrap();
        sample
            .app
            .invoke(
                Invoke::apply(&crate::actions::text_edit_action(
                    readme.clone(),
                    &text,
                    &format!("{text}saved\n"),
                ))
                .unwrap(),
            )
            .unwrap();
        sample
            .app
            .effect_confirmed(&CodeEditorEffect::Save {
                file: Some(readme),
                path: None,
            })
            .unwrap();
        let on_disk = std::fs::read_to_string(sample.root.join("README.md")).unwrap();
        assert!(on_disk.ends_with("saved\n"), "{on_disk}");
    }

    #[test]
    fn every_scenario_captures_over_the_sample_project() {
        for scenario in CodeEditorShell::scenarios()
            .iter()
            .copied()
            .filter(|scenario| *scenario != "terminal")
            .map(Some)
            .chain([None])
        {
            let mut app = TestApp::<CodeEditorShell>::capture(scenario)
                .unwrap_or_else(|error| panic!("{scenario:?}: {error}"));
            let (name, staging) = editor(&mut app, |app| {
                (app.active_buffer_name(), app.is_staging_for_test())
            });
            assert_eq!(name.as_deref(), Some("main.rs"), "{scenario:?}");
            assert!(!staging, "{scenario:?} staged");
        }
    }

    #[test]
    fn the_search_scenario_shows_real_hits_and_find_shows_its_options() {
        let mut search = TestApp::<CodeEditorShell>::capture(Some("search")).unwrap();
        assert!(editor(&mut search, |app| app.search_hit_count_for_test()) > 0);
        let mut options = TestApp::<CodeEditorShell>::capture(Some("find-options")).unwrap();
        assert!(editor(&mut options, |app| app.find_options_shown_for_test()));
    }

    #[test]
    fn every_command_is_published_as_an_effect() {
        let mut sample = sample();
        let resources = sample.app.harness().resources();
        let root = find(&resources, PROJECT_KIND).unwrap();
        for spec in CodeEditorEffect::specs() {
            assert!(
                root.actions.iter().any(|action| action.id == spec.id),
                "{} is published",
                spec.id
            );
        }
    }

    #[test]
    fn language_servers_follow_the_preference_and_never_run_in_a_capture() {
        let mut sample = sample();
        sample.open("src/main.rs");
        assert!(!editor(&mut sample.app, |app| app.lsp_enabled_for_test()));
        sample
            .app
            .effect(&CodeEditorEffect::ToggleLanguageServers)
            .unwrap();
        assert!(
            sample
                .app
                .settings::<crate::settings::CodeSettings>()
                .get()
                .language_servers
        );
        assert!(editor(&mut sample.app, |app| app.lsp_enabled_for_test()));

        let mut capture = TestApp::<CodeEditorShell>::capture(None).unwrap();
        assert!(!editor(&mut capture, |app| app.lsp_enabled_for_test()));
    }

    #[test]
    fn a_dock_opened_outside_an_event_is_laid_out_before_it_is_drawn() {
        let mut sample = sample();
        sample.open("src/main.rs");
        sample.app.effect(&CodeEditorEffect::GoToProblems).unwrap();
        sample.app.settle();
        let targets = sample.app.harness().targets();
        let panel = targets
            .get("bottom_panel")
            .copied()
            .expect("the dock is shown");
        let editor = targets.get("editor_view").copied().expect("the editor");
        assert!(panel.y0 >= editor.y1 - 0.5, "{panel:?} below {editor:?}");
    }
}
