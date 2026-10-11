//! Git client as a [`tilo_resource::Resource`]: the repository the record
//! opens and the shared projection of its history sync; working-tree status
//! and an in-flight transfer are live rows of the surface, and the diff
//! preferences are the person's settings, switched by the surface's effects.

use std::rc::Rc;

use ui::framework::{
    Command, EffectAnswer, EffectCx, EffectStatus, Fixture, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
};
use ui::headless::FrameHarness;
use ui::i18n::EmbeddedCatalog;
use ui::tilo_actions::Resource as PublishedResource;
use ui::tilo_resource::{Doc, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    GIT_CLIENT_ACTION_TYPE, GitClientAction, GitClientEffect, document_children, register,
    resolve_child,
};
use crate::app::GitClientApp;
use crate::doc::GitClientState;
use crate::model::{DiffMap, RepoModel};

/// The git client resource. Document state is the repository the record
/// opens and the shared repository projection; working-tree rows and
/// in-flight fetch/push are live.
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

    fn children(state: &GitClientState) -> Vec<PublishedResource> {
        document_children(&state.repo)
    }

    fn child(state: &GitClientState, id: &str) -> Option<PublishedResource> {
        resolve_child(&state.repo, id)
    }
}

/// The bundled repository a screenshot shows instead of whatever repository
/// the machine taking it has.
pub struct SampleRepository {
    pub model: RepoModel,
    pub diffs: DiffMap,
}

ui::embedded_catalog!(static CATALOG, "git-remote-running", ["en-US", "de-DE"]);

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

    /// `discard` presses "Discard file" on a changed file: the host asks to
    /// confirm the destructive effect before it runs.
    fn scenario_input(name: Option<&str>, input: &mut FrameHarness) -> Result<(), String> {
        if name == Some("discard") {
            let discard = input
                .a11y_node_named("Discard file changes")
                .ok_or("no \"Discard file\" button on the changed file")?;
            input.perform_a11y_action(discard, &ui::a11y::Action::Activate)?;
        }
        Ok(())
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

impl ResourceSurface<GitClient, GitClientEffect> for GitClientApp {
    fn present(&mut self, state: &Rc<GitClientState>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<GitClient>) {
        let sample = cx.fixture::<SampleRepository>();
        self.attach_control_actions(cx.actions());
        self.attach_dispatcher(cx.dispatcher(), cx.mode(), sample.as_deref());
        self.follow_preferences(cx.settings::<crate::settings::Settings>());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: GitClientEffect,
        cx: &mut EffectCx<GitClient, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.preference_statuses()
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
    use crate::settings::{DiffLayout, Settings};
    use ui::automerge::AutoCommit;
    use ui::framework::Effect as _;
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

    #[test]
    fn ordinary_primary_repository_declares_identity_in_native_tree_and_actions() {
        let (_dir, path) = scratch_repository("primary-identity");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        let resources = app.harness().resources();
        let root = resources
            .iter()
            .find(|resource| resource.id.as_str() == "git_client.repo")
            .expect("normal primary repository resource");
        let expected =
            ui::tilo_actions::ResourceSemanticIdentity::new("git_client.repo", "git_client.repo");
        assert_eq!(root.semantic_identity, expected);
        let actions = app.actions().unwrap();
        let primary = actions
            .iter()
            .find(|action| {
                action.address.resource.as_str() == "git_client.repo"
                    && action.address.action == ui::tilo_resource::APPLY
            })
            .expect("normal primary repository document action");
        assert_eq!(primary.semantic_identity, expected);
    }

    /// The repository door carries the projection's history, branches and
    /// remotes with their item effects; the working tree is not document
    /// state.
    #[test]
    fn the_repository_door_publishes_the_shared_projection() {
        let (model, _) = crate::sample::model();
        let mut doc = AutoCommit::new();
        SharedRepo::write(&mut doc, &SharedRepo::from_model(&model));
        doc.commit();

        let state = GitClient::read(&doc).unwrap();
        let published =
            ui::tilo_resource::publish::<GitClient>("git_client.repo", "renderer", &state);
        assert_eq!(published.kind, "git_client.repo");
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
    /// the scenario's fixture, the split layout, the screen and open states
    /// the surface's staging, and changed files are live rows
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
        assert!(rows.iter().any(|row| row["action"] == "stage-file"));

        let split = TestApp::<GitClientShell>::capture(Some("split")).unwrap();
        assert_eq!(
            split.settings::<Settings>().get().diff_layout,
            DiffLayout::SideBySide
        );

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

    #[test]
    fn painted_screen_swipes_use_the_live_view_actions_without_document_edits() {
        use winit::event::TouchPhase::{Cancelled, Ended, Started};
        fn control(
            node: &ui::headless::InspectionNode,
            target: &str,
        ) -> Option<ui::a11y::AccessNode> {
            fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<ui::a11y::AccessNode> {
                if node.action_target.as_deref() == Some(target) {
                    return Some(node.clone());
                }
                node.children.iter().find_map(|child| find(child, target))
            }
            node.semantics
                .as_ref()
                .and_then(|node| find(node, target))
                .or_else(|| {
                    node.children
                        .iter()
                        .find_map(|child| control(child, target))
                })
        }
        let (_dir, path) = scratch_repository("swipe-navigation");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.settle();
        let before = app.peer_document().get_heads();
        let start = control(&app.harness().inspection().root, "git.screen.history")
            .unwrap()
            .bounds
            .unwrap()
            .to_rect()
            .center();
        app.harness().touch(1, Started, start.x, start.y);
        app.harness().touch(1, Ended, start.x - 90.0, start.y);
        app.settle();
        assert!(
            control(&app.harness().inspection().root, "git.screen.history")
                .unwrap()
                .state
                .selected
        );
        let start = control(&app.harness().inspection().root, "git.screen.history")
            .unwrap()
            .bounds
            .unwrap()
            .to_rect()
            .center();
        app.harness().touch(2, Started, start.x, start.y);
        app.harness().touch(2, Cancelled, start.x - 90.0, start.y);
        app.settle();
        assert!(
            control(&app.harness().inspection().root, "git.screen.history")
                .unwrap()
                .state
                .selected
        );
        app.harness().touch(3, Started, start.x, start.y);
        app.harness().touch(3, Ended, start.x + 90.0, start.y);
        app.settle();
        assert!(
            control(&app.harness().inspection().root, "git.screen.changes")
                .unwrap()
                .state
                .selected
        );
        assert_eq!(
            app.peer_document().get_heads(),
            before,
            "navigation changes no collaborative document"
        );
    }

    /// The `discard` screenshot is the host asking to confirm a changed
    /// file's destructive `discard-file`, which its "Discard file" pressed.
    #[test]
    fn discarding_a_file_asks_the_host_to_confirm() {
        let mut discard = TestApp::<GitClientShell>::capture(Some("discard")).unwrap();
        let harness = discard.harness();
        let names: Vec<String> = harness
            .a11y_nodes()
            .iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect();
        let targets: Vec<String> = harness.targets().into_keys().collect();
        assert!(
            targets
                .iter()
                .any(|name| name.starts_with("ordinary-confirmation:"))
                || harness
                    .inspection_json()
                    .unwrap()
                    .contains("ordinary-confirmation:"),
            "the host shows its confirmation: {targets:?} {names:?}"
        );
    }

    /// A repository named on the command line is the record's, opened again
    /// on the next launch; its projection reaches the document.
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

        let mut reopened = app.reopen().unwrap();
        reopened.settle();
        let shown = reopened.harness().inspection_json().unwrap();
        assert!(
            shown.contains("new.txt"),
            "the record reopens its repository"
        );
    }

    /// Work on the repository reaches this machine's disk, so only the
    /// person at it may ask for it; the diff preferences are the person's
    /// settings and reach nothing of the machine.
    #[test]
    fn only_repository_work_is_device_only() {
        use ui::tilo_resource::Effect as _;
        let device_only = |id: &str| {
            GitClientEffect::declarations()
                .into_iter()
                .find(|decl| decl.id == id)
                .unwrap_or_else(|| panic!("{id} is declared"))
                .device_only
        };
        for id in ["commit", "stage-all", "fetch", "discard-file"] {
            assert!(device_only(id), "{id}");
        }
        for id in [
            "diff-layout",
            "syntax-highlight",
            "show-whitespace",
            "wrap-lines",
        ] {
            assert!(!device_only(id), "{id}");
        }
    }

    #[test]
    fn approved_project_check_is_a_cancellable_host_job_with_only_an_id_parameter() {
        let declaration = GitClientEffect::declarations()
            .into_iter()
            .find(|declaration| declaration.id == "run-approved-check")
            .expect("approved project check is on the actual repository effect surface");
        assert!(!declaration.device_only);
        assert_eq!(
            declaration.answers,
            ui::tilo_resource::effect::Answers::Later {
                cancellable: true,
                returns: true
            }
        );
        assert!(declaration.untrusted);
        assert_eq!(declaration.params.len(), 1);
        assert_eq!(declaration.params[0].name, "check_id");
        let check = GitClientEffect::RunApprovedCheck {
            check_id: "regression".into(),
        };
        let mut app = TestApp::<GitClientShell>::capture(Some("empty")).unwrap();
        let error = app
            .effect(&check)
            .expect_err("detached test app cannot manufacture admitted project custody");
        assert!(error.to_string().contains("approved"));
    }

    /// Undo takes back what the launch did in the record — opening the
    /// repository — but not the projection read from the repository on
    /// disk, which is nobody's edit.
    #[test]
    fn undoing_the_launch_keeps_the_repository_projection() {
        let (_dir, path) = scratch_repository("undo-repo");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.settle();
        assert_eq!(app.state().repo.name, "undo-repo");

        let mut undone = 0;
        while app.undo() {
            undone += 1;
            assert!(undone < 16, "the launch's steps run out");
        }
        app.settle();
        let state = app.state();
        assert!(undone > 0, "opening the repository is an undo step");
        assert_eq!(
            state.repository, None,
            "undo took back opening the repository"
        );
        assert_eq!(
            state.repo.name, "undo-repo",
            "the projection survives every undo"
        );
        assert_eq!(state.repo.commits.len(), 1);
    }

    /// The client's effects are published on the repository and on the
    /// items they act on, and every one reaches the repository on disk:
    /// staging a changed file through its row, committing, creating and
    /// checking out a branch, and deleting one only once confirmed.
    #[test]
    fn effects_reach_the_repository_on_disk() {
        use crate::actions::{GitClientEffect, branch_resource_id, change_resource_id};
        use crate::model::ChangeArea;

        let (_dir, path) = scratch_repository("effects-repo");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.settle();
        let surface = app.agent_surface();
        let offered = |resource: &str, action: &str| {
            surface
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["resource"] == resource && row["action"] == action)
        };
        for id in [
            "show-changes",
            "show-history",
            "show-branches",
            "stage-all",
            "unstage-all",
            "commit",
            "create-branch",
            "save-stash",
        ] {
            assert!(offered("git_client.repo", id), "{id}: {surface:#}");
        }
        let untracked = change_resource_id(ChangeArea::Untracked, "new.txt");
        for id in ["open-change", "stage-file", "discard-file"] {
            assert!(offered(&untracked, id), "{id}: {surface:#}");
            assert!(
                !offered("git_client.repo", id),
                "{id} is offered only bound"
            );
        }

        app.effect_on(
            &untracked,
            &GitClientEffect::StageFile {
                path: "new.txt".into(),
            },
        )
        .unwrap();
        let repo = git2::Repository::open(&path).unwrap();
        assert!(
            repo.index()
                .unwrap()
                .get_path(std::path::Path::new("new.txt"), 0)
                .is_some(),
            "the row's effect staged the file"
        );

        app.effect(&GitClientEffect::Commit {
            message: "Add new file".into(),
        })
        .unwrap();
        let head = repo.head().unwrap();
        let first = head.shorthand().unwrap().to_owned();
        assert_eq!(
            head.peel_to_commit().unwrap().summary().unwrap(),
            Some("Add new file")
        );
        assert!(
            app.effect(&GitClientEffect::Commit {
                message: "Nothing staged".into(),
            })
            .is_err(),
            "a commit with nothing staged fails its caller"
        );

        app.effect(&GitClientEffect::CreateBranch {
            name: "topic".into(),
        })
        .unwrap();
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "topic");
        app.settle();
        let first_row = branch_resource_id(&first);
        app.effect_on(
            &first_row,
            &GitClientEffect::CheckoutBranch {
                name: first.clone(),
            },
        )
        .unwrap();
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), first);

        app.settle();
        let topic = branch_resource_id("topic");
        let delete = GitClientEffect::DeleteBranch {
            name: "topic".into(),
        };
        assert!(
            app.effect_on(&topic, &delete).is_err(),
            "deleting a branch asks first"
        );
        assert!(repo.find_branch("topic", git2::BranchType::Local).is_ok());
        app.effect_on_confirmed(&topic, &delete).unwrap();
        assert!(repo.find_branch("topic", git2::BranchType::Local).is_err());
    }

    /// The published job creates a real checkout and the graphical surface
    /// and live resource collection reflect the resulting branch and path.
    #[test]
    fn linked_worktree_effect_reaches_disk_and_the_graphical_surface() {
        let (dir, path) = scratch_repository("worktree-repo");
        let destination = dir.path().join("agent-worktree");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.settle();
        let surface = app.agent_surface();
        assert!(
            surface
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["action"] == "create-worktree")
        );
        app.effect(&GitClientEffect::CreateWorktree {
            path: destination.to_str().unwrap().into(),
            branch: "codex/experiment".into(),
            start: "HEAD".into(),
        })
        .unwrap();
        app.settle();
        assert_eq!(
            git2::Repository::open(&destination)
                .unwrap()
                .head()
                .unwrap()
                .shorthand()
                .unwrap(),
            "codex/experiment"
        );
        app.effect(&GitClientEffect::ShowBranches).unwrap();
        let shown = app.harness().inspection_json().unwrap();
        assert!(
            shown.contains("codex/experiment"),
            "the branch is shown: {shown}"
        );
        assert!(
            shown.contains("New worktree branch"),
            "the creation form is reachable: {shown}"
        );
        assert!(
            shown.contains(destination.to_str().unwrap()),
            "the actual checkout path is shown: {shown}"
        );
        // Device paths stay out of the shared repository projection.
        assert!(app.state().repo.worktrees.is_empty());
        assert!(
            app.effect(&GitClientEffect::CreateWorktree {
                path: destination.to_str().unwrap().into(),
                branch: "codex/another".into(),
                start: "HEAD".into(),
            })
            .is_err()
        );
        assert!(
            git2::Repository::open(&path)
                .unwrap()
                .find_branch("codex/another", git2::BranchType::Local)
                .is_err()
        );
    }

    /// The app-bar layout control and the palette's effect switch the
    /// person's diff preference: kept beside the document, changing no
    /// document and on nobody's undo stack.
    #[test]
    fn diff_preferences_are_personal_settings() {
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

        let (_control_dir, control_path) = scratch_repository("preferences-controls");
        let mut app =
            TestApp::<GitClientShell>::launched(&[control_path.to_str().unwrap()]).unwrap();
        app.settle();
        // The launch is a document undo step; finish it before proving a
        // personal preference adds none. Undo retains the real repository's
        // live projection and device binding, as covered separately below.
        let mut launch_steps = 0;
        while app.undo() {
            launch_steps += 1;
            assert!(launch_steps < 16, "launch history is bounded");
        }
        assert!(
            launch_steps > 0,
            "the repository was opened through its real launch action"
        );
        app.settle();
        let control = bounds(&app.harness().inspection().root, "git.diff-layout.split")
            .expect("the app bar exports its layout control")
            .center();
        let before = app.peer_document().get_heads();
        app.harness().click_at(control.x, control.y);
        app.settle();
        let layout = |app: &TestApp<GitClientShell>| app.settings::<Settings>().get().diff_layout;
        assert_eq!(layout(&app), DiffLayout::SideBySide);
        assert!(!app.undo(), "a preference is no undo step");
        assert_eq!(layout(&app), DiffLayout::SideBySide);
        assert_eq!(
            app.peer_document().get_heads(),
            before,
            "no document change"
        );

        // The palette offers each preference as an effect on the repository,
        // labelled with what it switches to.
        let (_dir, path) = scratch_repository("preferences-repo");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.settle();
        let offered_layout = |app: &mut TestApp<GitClientShell>| {
            let palette = app.agent_surface();
            palette
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["resource"] == "git_client.repo" && row["action"] == "diff-layout")
                .map_or_else(
                    || panic!("the palette offers the layout: {palette:#}"),
                    |row| row["label"].clone(),
                )
        };
        assert_eq!(offered_layout(&mut app), "Diff layout: side-by-side");
        app.effect(&GitClientEffect::DiffLayout).unwrap();
        assert_eq!(layout(&app), DiffLayout::SideBySide);
        app.effect(&GitClientEffect::SyntaxHighlight).unwrap();
        app.effect(&GitClientEffect::ShowWhitespace).unwrap();
        assert!(
            app.settings::<Settings>().get().wrap_lines,
            "long lines wrap by default"
        );
        app.effect(&GitClientEffect::WrapLines).unwrap();
        let stored = app.settings::<Settings>().get();
        assert!(!stored.syntax_highlight);
        assert!(stored.show_whitespace);
        assert!(
            !stored.wrap_lines,
            "the wrapping effect persists the opposite preference"
        );
        app.effect(&GitClientEffect::WrapLines).unwrap();
        assert!(
            app.settings::<Settings>().get().wrap_lines,
            "wrapping can be enabled again"
        );

        // A preference stored by another window or device shows here.
        app.settings::<Settings>()
            .set(&Settings {
                diff_layout: DiffLayout::Unified,
                ..stored
            })
            .unwrap();
        app.settle();
        assert_eq!(offered_layout(&mut app), "Diff layout: side-by-side");
        app.effect(&GitClientEffect::DiffLayout).unwrap();
        assert_eq!(offered_layout(&mut app), "Diff layout: unified");
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
