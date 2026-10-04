//! Git client as a [`tilo_resource::Resource`]: the diff preferences, the
//! repository the record opens and the shared projection of its history
//! sync; working-tree status and an in-flight transfer are live rows of the
//! surface.

use std::rc::Rc;

use ui::framework::{
    Command, Fixture, LaunchInput, LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_actions::{ActionSpec, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{Doc, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    GIT_CLIENT_ACTION_TYPE, GitClientAction, document_bound_actions, document_children, register,
    resolve_child, root_local_operations,
};
use crate::app::GitClientApp;
use crate::doc::GitClientState;
use crate::model::{DiffMap, RepoModel};
use crate::settings::DiffLayout;

/// The git client resource. Document state is the diff preferences, the
/// repository the record opens and the shared repository projection;
/// working-tree rows and in-flight fetch/push are live.
pub struct GitClient;

impl Resource for GitClient {
    const KIND: &'static str = "git_client.repo";
    const LABEL: &'static str = "Repository";
    const ACTION_TYPE: &'static str = GIT_CLIENT_ACTION_TYPE;

    type State = GitClientState;
    type Action = GitClientAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<GitClientState, ModelError> {
        Ok(GitClientState::from_doc(doc))
    }

    fn bound_actions(state: &GitClientState) -> Vec<ActionSpec> {
        document_bound_actions(state.settings)
    }

    fn children(state: &GitClientState) -> Vec<PublishedResource> {
        document_children(&state.repo)
    }

    fn child(state: &GitClientState, id: &str) -> Option<PublishedResource> {
        resolve_child(&state.repo, id)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // View navigation and working-tree / branch / stash Effects. Per-remote
        // fetch/pull/push ride remote children from [`children`].
        root_local_operations()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// The bundled repository a screenshot shows instead of whatever repository
/// the machine taking it has.
pub struct SampleRepository {
    pub model: RepoModel,
    pub diffs: DiffMap,
}

/// The client's messages, in every host that mounts it.
static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[
        ("en-US", include_str!("../locales/en-US.ftl")),
        ("de-DE", include_str!("../locales/de-DE.ftl")),
    ],
    "git-remote-running",
)
.with_system_locale();

/// Every `--scenario` the screenshot renderer stages.
pub const SCENARIOS: &[&str] = &[
    "changes", "split", "history", "branches", "commit", "discard", "empty", "narrow",
];

/// Framework app: the git client widget over [`GitClient`].
pub struct GitClientShell;

impl ResourceApp for GitClientShell {
    type Resource = GitClient;
    const ID: &'static str = "so.tilo.git-client";
    const TITLE: &'static str = "Git";
    const ICON: Option<&'static str> = Some("git-client");
    /// The repository to open, by path.
    const ARGUMENTS: usize = 1;

    fn label(state: &GitClientState) -> String {
        crate::actions::repo_label(&state.repo)
    }

    fn view(cx: &mut ViewCx<GitClient>) -> Element {
        cx.surface(GitClientApp::new())
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn commands() -> &'static [Command] {
        &[Command {
            option: "--stage-commit",
            run: stage_and_commit,
        }]
    }

    /// A path on the command line opens that repository.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<GitClient>> {
        input
            .argument()
            .map(|path| {
                std::path::absolute(path)
                    .unwrap_or_else(|_| path.into())
                    .to_string_lossy()
                    .into_owned()
            })
            .and_then(|path| Invoke::apply(&GitClientAction::OpenRepository { path }).ok())
            .into_iter()
            .collect()
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario == Some("narrow") {
            (680, 860)
        } else {
            (1280, 800)
        }
    }

    /// Every scenario but the first-run `empty` one shows the bundled
    /// repository.
    fn fixture(fixture: &mut Fixture<GitClient>) {
        if fixture.scenario() != Some("empty") {
            let (model, diffs) = crate::sample::model();
            fixture.provide(SampleRepository { model, diffs });
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<GitClient>> {
        if name == Some("split") {
            return Invoke::apply(&crate::actions::set_diff_layout_action(
                DiffLayout::SideBySide,
            ))
            .into_iter()
            .collect();
        }
        Vec::new()
    }
}

/// `--stage-commit MESSAGE`: stage everything in the repository around the
/// working directory and commit it.
fn stage_and_commit(message: &str) -> Result<(), String> {
    let path = std::env::current_dir().map_err(|error| error.to_string())?;
    crate::repo::stage_all(&path).map_err(|error| format!("could not stage changes: {error}"))?;
    let id = crate::repo::commit(&path, message)
        .map_err(|error| format!("could not create commit: {error}"))?;
    println!("git_client: committed {id} to {}", path.display());
    Ok(())
}

impl ResourceSurface<GitClient> for GitClientApp {
    fn present(&mut self, state: &Rc<GitClientState>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<GitClient>) {
        let sample = cx.fixture::<SampleRepository>();
        self.attach_dispatcher(cx.dispatcher(), cx.mode(), sample.as_deref());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.working_tree_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.working_tree_row(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{SharedRepo, shared_fingerprint};
    use crate::settings::Settings;
    use ui::automerge::AutoCommit;
    use ui::framework::testing::TestApp;

    /// A repository with one commit and one untracked file, in a directory
    /// named `name`.
    fn scratch_repository(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::create_dir(&path).unwrap();
        let repo = git2::Repository::init(&path).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
        std::fs::write(path.join("file.txt"), "one\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Test User", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "first commit", &tree, &[])
            .unwrap();
        std::fs::write(path.join("new.txt"), "new\n").unwrap();
        (dir, path)
    }

    /// The repository door carries the preferences, the views and Effects,
    /// and the projection's history, branches and remotes; the working tree
    /// is not document state.
    #[test]
    fn the_repository_door_publishes_the_shared_projection() {
        let (model, _) = crate::sample::model();
        let mut doc = AutoCommit::new();
        Settings::default().write(&mut doc);
        SharedRepo::write(&mut doc, &SharedRepo::from_model(&model));
        doc.commit();

        let state = GitClient::read(&doc).unwrap();
        let published =
            ui::tilo_resource::publish::<GitClient>("git_client.repo", "renderer", &state);
        assert_eq!(published.kind, "git_client.repo");
        for id in ["diff-layout", "show-changes", "commit", "stage-all"] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} is on the repository"
            );
        }
        for id in [
            "git_client.branches",
            "git_client.remotes",
            "git_client.history",
        ] {
            assert!(
                published
                    .children
                    .iter()
                    .any(|child| child.id.as_str() == id),
                "{id} is a child"
            );
        }
        assert!(
            published
                .children
                .iter()
                .all(|child| child.id.as_str() != "git_client.changes"),
            "working-tree status is not in the document"
        );
        let remote = published
            .children
            .iter()
            .find(|child| child.id.as_str() == "git_client.remotes")
            .and_then(|collection| collection.children.first())
            .expect("sample remote");
        assert!(remote.actions.iter().any(|action| action.id == "fetch"));
        assert_eq!(
            shared_fingerprint(&model),
            shared_fingerprint(&SharedRepo::from_model(&model).into_model()),
            "the shared fingerprint ignores working-tree rows"
        );
    }

    /// Every screenshot is the framework's capture: the sample repository is
    /// the scenario's fixture, the split layout its action, the screen and
    /// open states the surface's staging, and changed files are live rows
    /// that only view or run Effects.
    #[test]
    fn captures_stage_the_sample_and_publish_live_rows() {
        let mut changes = TestApp::<GitClientShell>::capture(Some("changes")).unwrap();
        assert_eq!(changes.harness().inspection().viewport, [1280, 800]);
        let surface = changes.agent_surface();
        let rows: Vec<_> = surface
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["resource_kind"] == "git_client.diff")
            .collect();
        assert!(!rows.is_empty(), "changed files are published: {surface:#}");
        assert!(
            rows.iter()
                .all(|row| row["operation"] == "View" || row["operation"] == "Effect"),
            "{rows:#?}"
        );
        assert!(rows.iter().any(|row| row["action"] == "stage"));

        let split = TestApp::<GitClientShell>::capture(Some("split")).unwrap();
        assert_eq!(split.state().settings.diff_layout, DiffLayout::SideBySide);

        let mut narrow = TestApp::<GitClientShell>::capture(Some("narrow")).unwrap();
        assert_eq!(narrow.harness().inspection().viewport, [680, 860]);

        let mut empty = TestApp::<GitClientShell>::capture(Some("empty")).unwrap();
        assert!(
            empty
                .harness()
                .inspection_json()
                .unwrap()
                .contains("Repository path")
        );
    }

    /// A repository named on the command line is the record's, opened again
    /// on the next launch; its projection reaches the document as content
    /// from the repository, which no undo takes back, and opening writes no
    /// preferences.
    #[test]
    fn a_launched_repository_is_the_records_and_its_projection_syncs() {
        let (_dir, path) = scratch_repository("scratch-repo");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.settle();
        let state = app.state();
        assert_eq!(
            state.repository.as_deref(),
            Some(path.to_str().unwrap()),
            "the launch opened the repository in the record"
        );
        assert_eq!(
            state.repo.name, "scratch-repo",
            "the projection was written"
        );
        assert_eq!(state.repo.commits.len(), 1);
        assert!(
            !app.read_document(|doc| {
                use ui::automerge::ReadDoc;
                doc.get(&ui::automerge::ROOT, crate::settings::keys::SETTINGS)
                    .ok()
                    .flatten()
                    .is_some()
            }),
            "opening writes no preferences"
        );

        // A preference is one undo step.
        app.call("diff-layout", serde_json::json!({})).unwrap();
        assert_eq!(app.state().settings.diff_layout, DiffLayout::SideBySide);
        assert!(app.undo());
        assert_eq!(app.state().settings.diff_layout, DiffLayout::Unified);
        assert_eq!(
            app.state().repo.name,
            "scratch-repo",
            "the projection is nobody's undo step"
        );

        let mut reopened = app.reopen().unwrap();
        reopened.settle();
        let shown = reopened.harness().inspection_json().unwrap();
        assert!(
            shown.contains("new.txt"),
            "the record reopens its repository"
        );
    }

    /// The app-bar layout control submits the preference action the
    /// palette offers: one undo step, presented back to the control.
    #[test]
    fn the_layout_control_edits_through_the_framework_door() {
        fn bounds(node: &ui::headless::InspectionNode, target: &str) -> Option<vello::kurbo::Rect> {
            fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<vello::kurbo::Rect> {
                if node.action_target.as_deref() == Some(target) {
                    return node.bounds.map(ui::a11y::AccessBounds::to_rect);
                }
                node.children.iter().find_map(|child| find(child, target))
            }
            node.semantics
                .as_ref()
                .and_then(|semantic| find(semantic, target))
                .or_else(|| node.children.iter().find_map(|child| bounds(child, target)))
        }

        let mut app = TestApp::<GitClientShell>::capture(None).unwrap();
        let palette = app.agent_surface();
        let offered = palette
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["resource"] == "git_client.repo" && row["action"] == "diff-layout")
            .expect("the palette offers the layout");
        assert_eq!(offered["label"], "Diff layout: side-by-side");

        let control = bounds(&app.harness().inspection().root, "git.diff-layout")
            .expect("the app bar exports its layout control")
            .center();
        app.harness().click_at(control.x, control.y);
        app.settle();
        assert_eq!(app.state().settings.diff_layout, DiffLayout::SideBySide);
        assert!(app.undo(), "the control's edit is one undo step");
        assert_eq!(app.state().settings.diff_layout, DiffLayout::Unified);
    }

    /// Every host that mounts the client loads its messages.
    #[test]
    fn the_catalog_resolves_the_remote_messages() {
        GitClientShell::catalog().unwrap().ensure();
        for (key, value) in [
            ("git-remote-running", ui::i18n!("git-remote-running")),
            ("git-remote-busy", ui::i18n!("git-remote-busy")),
            (
                "git-remote-worker-stopped",
                ui::i18n!("git-remote-worker-stopped"),
            ),
        ] {
            assert_ne!(key, value);
            assert!(!value.is_empty());
        }
    }
}
