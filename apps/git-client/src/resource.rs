//! Git client as a resource: the record is the repository it opens and
//! what collaborators see of its history ([`GitClient`]); working-tree
//! status, the history the window shows and an in-flight transfer are live
//! rows of the surface, and the diff preferences are the person's settings,
//! switched by the surface's effects.

use std::rc::Rc;

use ui::framework::{
    Command, EffectAnswer, EffectCx, EffectStatus, Fixture, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
};
use ui::headless::FrameHarness;
use ui::i18n::EmbeddedCatalog;

use crate::i18n::t;
use ui::tilo_resource::{self, Atom, Invoke, Model, resource};
use ui::widget::Element;

use crate::actions::GitClientEffect;
use crate::app::GitClientApp;
use crate::model::{DiffMap, RepoModel};
use crate::shared::SharedRepo;

/// The record: the repository it opens and what collaborators see of it.
/// Working-tree rows and in-flight fetch/push are live.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct GitClient {
    /// Repository
    ///
    /// The path of the repository the record opens; empty until one was
    /// opened, when the client looks around its working directory.
    pub repository: Atom,
    /// Shared history
    ///
    /// Branches, history, remotes and identity as the repository on disk
    /// last held them, for collaborators and past versions.
    #[model(readonly)]
    pub shared: SharedRepo,
}

impl GitClient {
    /// The repository the record opens, if one was opened.
    pub fn opened(&self) -> Option<&str> {
        Some(self.repository.0.as_str()).filter(|path| !path.is_empty())
    }
}

#[resource(kind = "git_client.repo", label = "Repository")]
impl GitClient {
    /// Record what the repository on disk holds now
    ///
    /// Its branches, history, remotes and identity, for collaborators and
    /// past versions. Read from the repository, nobody's edit.
    #[action(external)]
    pub fn record_repository(
        &mut self,
        #[param("The repository's branches, history, remotes and identity")] shared: SharedRepo,
    ) {
        self.shared = shared;
    }
}

/// The bundled repository a screenshot shows instead of whatever repository
/// the machine taking it has.
pub struct SampleRepository {
    pub model: RepoModel,
    pub diffs: DiffMap,
}

ui::embedded_catalog!(static CATALOG, ["en-US", "de-DE"]);

/// Every `--scenario` the screenshot renderer stages.
pub const SCENARIOS: &[&str] = &[
    "changes", "split", "history", "branches", "commit", "conflict", "switch", "discard", "empty",
    "narrow",
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

    fn label(state: &GitClient) -> String {
        crate::actions::repo_label(&state.shared.clone().into_model())
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
            .and_then(|path| Invoke::edit(&GitClient::paths().repository.set(Atom(path))).ok())
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
        if fixture.scenario() == Some("empty") {
            return;
        }
        // The conflict capture is a repository mid-merge: one file both
        // sides changed, waiting for the person to say which one wins.
        let (model, diffs) = if fixture.scenario() == Some("conflict") {
            crate::sample::model_with_conflict()
        } else {
            crate::sample::model()
        };
        fixture.provide(SampleRepository { model, diffs });
    }

    /// `discard` presses "Discard file" on a changed file: the host asks to
    /// confirm the destructive effect before it runs.
    fn scenario_input(name: Option<&str>, input: &mut FrameHarness) -> Result<(), String> {
        if name == Some("discard") {
            // The name the button carries in whatever language the capture
            // is running in, not the one this source file happens to spell:
            // under `--pseudo-locale` the two differ, and a scenario that
            // looks for English finds no button at all.
            let discard = input
                .a11y_node_named(&t("git-discard-file-name"))
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
    fn present(&mut self, state: &Rc<GitClient>) {
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
    use crate::settings::{DiffLayout, Settings};
    use crate::shared::{SharedRepo, shared_fingerprint};
    use ui::framework::Effect as _;
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::Replica;

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

    /// A repository whose working tree is mid-merge: `file.txt` was rewritten
    /// on both sides, so it waits for the person to say which one wins.
    fn conflicted_repository(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let (dir, path) = scratch_repository(name);
        std::fs::remove_file(path.join("new.txt")).unwrap();
        crate::repo::create_branch(&path, "feature", None, true).unwrap();
        std::fs::write(
            path.join("file.txt"),
            "one\ntheirs\ntheirs again\ntheirs once more\n",
        )
        .unwrap();
        crate::repo::stage_file(&path, "file.txt").unwrap();
        crate::repo::commit(&path, "their side").unwrap();
        crate::repo::checkout_branch(&path, "master").unwrap();
        std::fs::write(path.join("file.txt"), "one\nours\n").unwrap();
        crate::repo::stage_file(&path, "file.txt").unwrap();
        crate::repo::commit(&path, "our side").unwrap();
        {
            let repo = git2::Repository::open(&path).unwrap();
            let tip = repo
                .find_branch("feature", git2::BranchType::Local)
                .unwrap()
                .into_reference()
                .peel_to_commit()
                .unwrap();
            let annotated = repo.find_annotated_commit(tip.id()).unwrap();
            repo.merge(&[&annotated], None, None).unwrap();
            assert!(
                repo.index().unwrap().has_conflicts(),
                "the fixture is conflicted"
            );
        }
        (dir, path)
    }

    /// The only file a merge left conflicted is the change the window shows,
    /// and the choice above it says what each side costs before it is made.
    /// `o` keeps our side from the keyboard, which settles the file.
    #[test]
    fn a_conflict_says_what_each_side_costs_and_settles_by_key() {
        let (_dir, path) = conflicted_repository("conflict-cost");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.settle();

        let labels: Vec<String> = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect();
        let note = labels
            .iter()
            .find(|label| label.contains("Both sides changed this file"))
            .unwrap_or_else(|| panic!("the choice names its cost: {labels:?}"));
        assert!(
            note.contains("3 lines of theirs") && note.contains("one of your lines"),
            "each side's cost is named: {note:?}"
        );
        for expected in [t("git-conflict-take-ours"), t("git-conflict-take-theirs")] {
            assert!(
                app.harness().a11y_node_named(&expected).is_some(),
                "{expected:?} is offered"
            );
        }
        assert!(
            app.harness()
                .a11y_node_named(&t("git-stage-file"))
                .is_none(),
            "the markers are not something to stage"
        );

        app.harness().key("o");
        app.settle();
        assert!(
            !git2::Repository::open(&path)
                .unwrap()
                .index()
                .unwrap()
                .has_conflicts(),
            "keeping our side settled the conflict"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("file.txt")).unwrap(),
            "one\nours\n"
        );
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

    /// The repository door carries what the window shows of the history,
    /// branches and remotes with their item effects; the record holds only
    /// the repository it opens and the projection, never the working tree.
    #[test]
    fn the_repository_door_publishes_what_the_window_shows() {
        let (model, _) = crate::sample::model();
        let mut app = TestApp::<GitClientShell>::capture(Some("history")).unwrap();
        let resources = app.harness().resources();
        let published = resources
            .iter()
            .find(|resource| resource.id.as_str() == "git_client.repo")
            .expect("the repository door");
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
        let stored = <GitClient as ui::tilo_resource::Resource>::surface();
        let fields = serde_json::to_string(&stored.fields).unwrap();
        for absent in ["staged", "unstaged", "untracked"] {
            assert!(!fields.contains(absent), "{absent} is not stored: {fields}");
        }
    }

    /// Two devices that record the repository and open it at once
    /// converge; recording is nobody's undo step.
    #[test]
    fn the_record_converges_and_recording_is_no_undo_step() {
        let (model, _) = crate::sample::model();
        let mut ours = Replica::<GitClient>::new();
        let mut theirs = ours.fork();
        ours.apply(
            Invoke::edit(
                &GitClient::paths()
                    .repository
                    .set(Atom("/srv/repos/tilo".into())),
            )
            .unwrap(),
        )
        .unwrap();
        theirs
            .apply(GitClient::RECORD_REPOSITORY.with(SharedRepo::from_model(&model)))
            .unwrap();
        ours.sync_with(&mut theirs).unwrap();
        assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
        let state = ours.state().unwrap();
        assert_eq!(state.opened(), Some("/srv/repos/tilo"));
        assert_eq!(state.shared, SharedRepo::from_model(&model));
        assert!(
            ours.apply(
                Invoke::edit(&GitClient::paths().shared.set(SharedRepo::default())).unwrap()
            )
            .is_err(),
            "only the repository on disk writes the projection"
        );
    }

    /// Opening a repository is history the person reads as the client's
    /// own sentence; recording the projection is not theirs.
    #[test]
    fn history_entries_render_as_the_apps_sentences() {
        use ui::tilo_actions::{ActionInvoker, ActorKind};

        let mut app = TestApp::<GitClientShell>::capture(Some("empty")).unwrap();
        let agent = ActionInvoker::actor("agent:claude")
            .with_kind(ActorKind::Agent)
            .acting_for("ada");
        let open = GitClient::paths()
            .repository
            .set(Atom("/srv/repos/tilo".into()));
        app.call_by(
            agent,
            <GitClient as ui::tilo_resource::Resource>::KIND,
            "field-edit",
            serde_json::json!({ "edit": open }),
        )
        .unwrap();
        assert_eq!(app.state().opened(), Some("/srv/repos/tilo"));
        let entries: Vec<_> = app
            .history()
            .into_iter()
            .filter(|entry| {
                entry
                    .invoker
                    .as_ref()
                    .is_some_and(|invoker| invoker.actor == "agent:claude")
            })
            .collect();
        let sentences = app.sentences(&entries);
        assert_eq!(sentences, ["Opened the repository /srv/repos/tilo"]);
        let invoker = entries[0].invoker.as_ref().unwrap();
        assert_eq!(
            (invoker.kind, invoker.on_behalf_of.as_deref()),
            (ActorKind::Agent, Some("ada"))
        );
    }

    ui::action_contract_tests! {
        seed: ui::automerge::AutoCommit::new(),
        registry: {
            let mut registry = ui::tilo_actions::ActionRegistry::new();
            <GitClient as ui::tilo_resource::Resource>::register(&mut registry);
            registry
        },
        resources: |_| TestApp::<GitClientShell>::capture(Some("history")).unwrap().harness().resources(),
        palette: ["Fetch remote", "Open commit"],
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

    /// The window bar's views switch the screens through the client's view
    /// actions, changing no collaborative document.
    #[test]
    fn the_bar_views_switch_screens_without_document_edits() {
        let (_dir, path) = scratch_repository("bar-navigation");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.harness().present_in_window_bar();
        app.settle();
        let before = app.peer_document().get_heads();
        let screen = |app: &mut TestApp<GitClientShell>| {
            app.surface(|client: &mut GitClientApp| client.window_toolbar())
                .and_then(|toolbar| toolbar.views)
                .map(|views| views.chosen)
        };
        assert_eq!(screen(&mut app).as_deref(), Some("git.screen.changes"));
        for target in [
            "git.screen.history",
            "git.screen.branches",
            "git.screen.changes",
        ] {
            app.harness()
                .press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: target.into() });
            app.settle();
            assert_eq!(screen(&mut app).as_deref(), Some(target));
        }
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
            state.opened(),
            Some(path.to_str().unwrap()),
            "the launch opened the repository in the record"
        );
        assert_eq!(
            state.shared.name, "scratch-repo",
            "the projection was written"
        );
        assert_eq!(state.shared.commits.len(), 1);

        let mut reopened = app.reopen().unwrap();
        reopened.settle();
        let shown = reopened.harness().inspection_json().unwrap();
        assert!(
            shown.contains("new.txt"),
            "the record reopens its repository"
        );
    }

    /// Work on the repository reaches outside the record, so the person's
    /// answers decide it like any outside effect; the diff preferences only
    /// read; choosing the repository opens a chooser for the person.
    #[test]
    fn repository_work_is_an_outside_effect_and_choosing_is_attended() {
        use ui::tilo_resource::Effect as _;
        let declaration = |id: &str| {
            GitClientEffect::declarations()
                .into_iter()
                .find(|decl| decl.id == id)
                .unwrap_or_else(|| panic!("{id} is declared"))
        };
        for id in ["commit", "stage-all", "fetch", "discard-file"] {
            let work = declaration(id);
            assert_eq!(
                work.action_kind,
                ui::tilo_actions::ActionKind::Outside,
                "{id}"
            );
            assert!(!work.attended, "{id}");
        }
        for id in [
            "diff-layout",
            "syntax-highlight",
            "show-whitespace",
            "wrap-lines",
        ] {
            assert_eq!(
                declaration(id).action_kind,
                ui::tilo_actions::ActionKind::Read,
                "{id}"
            );
        }
        assert!(declaration("choose-repository").attended);
    }

    #[test]
    fn approved_project_check_is_a_cancellable_host_job_with_only_an_id_parameter() {
        let declaration = GitClientEffect::declarations()
            .into_iter()
            .find(|declaration| declaration.id == "run-approved-check")
            .expect("approved project check is on the actual repository effect surface");
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
        assert_eq!(app.state().shared.name, "undo-repo");

        let mut undone = 0;
        while app.undo() {
            undone += 1;
            assert!(undone < 16, "the launch's steps run out");
        }
        app.settle();
        let state = app.state();
        assert!(undone > 0, "opening the repository is an undo step");
        assert_eq!(
            state.opened(),
            None,
            "undo took back opening the repository"
        );
        assert_eq!(
            state.shared.name, "undo-repo",
            "the projection survives every undo"
        );
        assert_eq!(state.shared.commits.len(), 1);
    }

    /// A repository opens by action, as typing its path or choosing its
    /// folder does: an agent (or a screenshot plan) names the path.
    #[test]
    fn a_repository_opens_by_action_and_a_missing_one_says_so() {
        use crate::actions::GitClientEffect;

        let (_dir, path) = scratch_repository("opened-by-action");
        let mut app = TestApp::<GitClientShell>::new().unwrap();
        app.settle();
        assert!(
            app.effect(&GitClientEffect::OpenRepository {
                path: "/no/such/repository".into(),
            })
            .is_err(),
            "no repository there"
        );
        app.effect(&GitClientEffect::OpenRepository {
            path: path.to_string_lossy().into_owned(),
        })
        .unwrap();
        app.settle();
        assert_eq!(app.state().shared.name, "opened-by-action");
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
        assert!(
            !serde_json::to_string(&app.state().shared)
                .unwrap()
                .contains(destination.to_str().unwrap())
        );
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

    /// A repository with one commit, the branch `feature` beside the one
    /// HEAD is on, and no uncommitted work.
    fn clean_repository(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let (dir, path) = scratch_repository(name);
        std::fs::remove_file(path.join("new.txt")).unwrap();
        let repo = git2::Repository::open(&path).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &head, false).unwrap();
        (dir, path)
    }

    fn local_branches(path: &std::path::Path) -> Vec<String> {
        git2::Repository::open(path)
            .unwrap()
            .branches(Some(git2::BranchType::Local))
            .unwrap()
            .map(|branch| branch.unwrap().0.name().unwrap().unwrap().to_owned())
            .collect()
    }

    fn head_of(path: &std::path::Path) -> String {
        git2::Repository::open(path)
            .unwrap()
            .head()
            .unwrap()
            .shorthand()
            .unwrap()
            .to_owned()
    }

    /// The question a dirty switch asks is answered from the keyboard: Enter
    /// on the branch opens it, and Enter again takes the answer that keeps
    /// the work — parking it on the stash — because that is the one the sheet
    /// offers as its default. Nothing needed the pointer.
    #[test]
    fn the_switch_question_is_answered_from_the_keyboard() {
        use crate::actions::GitClientEffect;

        let (_dir, path) = clean_repository("keyboard-switch");
        std::fs::write(path.join("file.txt"), "one\nedited\n").unwrap();
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.settle();
        let from = head_of(&path);

        app.effect_on(
            &crate::actions::branch_resource_id("feature"),
            &GitClientEffect::CheckoutBranch {
                name: "feature".into(),
            },
        )
        .unwrap();
        app.settle();
        assert_eq!(
            head_of(&path),
            from,
            "uncommitted work asks before the branch moves"
        );
        assert!(
            app.harness().a11y_node_named(&t("git-cancel")).is_some(),
            "the question and its answers are what a reader hears"
        );

        app.harness().key("Enter");
        app.settle();
        assert_eq!(head_of(&path), "feature", "Enter took the sheet's answer");
        assert_eq!(
            std::fs::read_to_string(path.join("file.txt")).unwrap(),
            "one\n",
            "the parked edits left the working tree"
        );
    }

    /// Escape is the way out of a question, from the keyboard, as it is for
    /// every other sheet.
    #[test]
    fn escape_puts_a_destructive_question_away() {
        let (_dir, path) = clean_repository("keyboard-escape");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.effect(&crate::actions::GitClientEffect::ShowBranches)
            .unwrap();
        app.settle();
        // The branches page opens on its first local branch, so Delete names
        // the branch the page is showing.
        assert_eq!(
            local_branches(&path).first().map(String::as_str),
            Some("feature")
        );
        app.harness().key("Delete");
        app.settle();
        assert!(
            app.harness()
                .a11y_node_named(&t("git-delete-branch"))
                .is_some(),
            "the sheet asks, naming the branch"
        );
        app.harness().key("Escape");
        app.settle();
        assert!(
            local_branches(&path).iter().any(|name| name == "feature"),
            "Escape answered the question: the branch is still there"
        );
    }

    /// Deleting a branch asks a question whose default answer is the way out:
    /// Enter alone cancels, and the arrow keys reach the destructive answer
    /// for someone who means it.
    #[test]
    fn a_destructive_answer_is_never_what_enter_fires() {
        let (_dir, path) = clean_repository("keyboard-delete");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.effect(&crate::actions::GitClientEffect::ShowBranches)
            .unwrap();
        app.settle();
        app.harness().key("Delete");
        app.settle();
        app.harness().key("Enter");
        app.settle();
        assert!(
            local_branches(&path).iter().any(|name| name == "feature"),
            "Enter on a destructive question is its way out"
        );

        app.harness().key("Delete");
        app.settle();
        app.harness().key("ArrowRight");
        app.harness().key("ArrowRight");
        app.harness().key("Enter");
        app.settle();
        assert!(
            !local_branches(&path).iter().any(|name| name == "feature"),
            "two arrows and Enter take the answer the person chose"
        );
    }

    /// A reader answers the same question the pointer and the keyboard
    /// answer: the sheet's answers are buttons with their names, and
    /// activating one does what pressing it does.
    #[test]
    fn a_reader_answers_the_same_question() {
        use ui::a11y::Action;

        let (_dir, path) = clean_repository("reader-delete");
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.effect(&crate::actions::GitClientEffect::ShowBranches)
            .unwrap();
        app.settle();
        app.harness().key("Delete");
        app.settle();
        let titled = |node: &ui::accesskit::Node| {
            node.label()
                .is_some_and(|label| label.starts_with("Delete feature?"))
        };
        let question = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| titled(node))
            .map(|(_, node)| node.label().unwrap_or_default().to_owned());
        let question = question.expect("the question is read out, naming the branch");
        assert!(
            question.contains("only this branch holds are lost"),
            "a reader hears what answering costs, not just what is asked: {question:?}"
        );
        assert!(
            app.harness().a11y_node_named(&t("git-cancel")).is_some(),
            "the way out is one of the answers a reader can reach"
        );
        let answer = app
            .harness()
            .a11y_node_named(&t("git-delete-branch"))
            .expect("the destructive answer is a named button");
        app.harness()
            .perform_a11y_action(answer, &Action::Activate)
            .unwrap();
        app.settle();
        assert!(
            !local_branches(&path).iter().any(|name| name == "feature"),
            "activating the named answer deleted the branch"
        );
    }

    /// Every destructive action the client publishes carries a sentence
    /// saying what it costs. The palette, a script and an agent ask through
    /// the action itself, and the sheet they get shows the action's own
    /// description under its question — an action documented as its own
    /// label asks "Discard file changes?" and then says nothing at all about
    /// what is about to be lost.
    #[test]
    fn a_destructive_action_says_what_it_loses() {
        use ui::tilo_actions::RiskLevel;

        let (_dir, path) = scratch_repository("destructive-docs");
        // A second branch, so the client publishes deleting one too.
        crate::repo::create_branch(&path, "feature", None, false).unwrap();
        let mut app = TestApp::<GitClientShell>::launched(&[path.to_str().unwrap()]).unwrap();
        app.settle();
        let actions = app.actions().unwrap();
        let destructive: Vec<_> = actions
            .iter()
            .filter(|action| action.contract.risk == RiskLevel::Destructive)
            .collect();
        assert!(
            destructive.len() >= 2,
            "the client publishes its destructive acts: {:?}",
            actions
                .iter()
                .map(|action| action.action_label.as_str())
                .collect::<Vec<_>>()
        );
        for action in destructive {
            let description = action
                .description
                .as_deref()
                .unwrap_or_else(|| panic!("{} says nothing", action.action_label));
            // What the sheet shows is the description without the heading it
            // already has as its question.
            let body = description
                .trim()
                .strip_prefix(action.action_label.trim())
                .unwrap_or(description)
                .trim();
            assert!(
                body.split_whitespace().count() >= 6,
                "{} asks without saying what it costs: {body:?}",
                action.action_label
            );
        }
    }

    /// The window bar's layout item and the palette's effect switch the
    /// person's diff preference: kept beside the document, changing no
    /// document and on nobody's undo stack.
    #[test]
    fn diff_preferences_are_personal_settings() {
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
        app.harness().present_in_window_bar();
        app.settle();
        let before = app.peer_document().get_heads();
        app.harness()
            .press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
                id: "git.diff-layout.split".into(),
            });
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

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::GitClientShell::scenarios() {
            let mut app = TestApp::<super::GitClientShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
