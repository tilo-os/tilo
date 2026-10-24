//! The launcher through the production providers that fill it: people's
//! presence, the notifications addressed to the person, where the windows
//! were, and the installed apps.

use std::rc::Rc;

use automerge::AutoCommit;
use tilo_desktop::notifications::{Addressed, Notification};
use tilo_desktop_model::{Collaborator, WorkspaceId};

use super::*;
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::search::providers::applications::ApplicationsProvider;
use crate::shell::search::providers::happening_now::workspace_of;
use crate::shell::search::providers::{ContinueProvider, PeopleProvider, WaitingOnYouProvider};
use crate::shell::search::{recent_places, usage};
use crate::shell::state::WorkspaceTree;

const NOTES: &str = "so.tilo.notes";

/// Home in front and Research behind it and two installed apps; nobody else
/// anywhere, nothing addressed to the person, no window anywhere yet.
struct Desk {
    chrome: Chrome,
    home: WorkspaceId,
    research: WorkspaceId,
    registry: SearchRegistry,
    doc: AutoCommit,
    _apps: tempfile::TempDir,
    _env: tilo_test_env::ScopedEnv,
}

fn install_app(root: &std::path::Path, package_id: &str, name: &str, category: &str) {
    use std::fmt::Write as _;
    let payload = root.join("payloads").join(package_id);
    std::fs::create_dir_all(payload.join("bin")).expect("payload dir");
    let manifest_path = payload.join("manifest.toml");
    std::fs::write(
        &manifest_path,
        crate::shell::test_support::canonical_manifest(
            &format!(
                "id = \"{package_id}\"\nname = \"{name}\"\n\
                 version = \"1.0.0\"\nkind = \"app\"\ncategory = \"{category}\""
            ),
            "[entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n",
        ),
    )
    .expect("manifest");
    let registry = root.join("registry.toml");
    let mut existing = std::fs::read_to_string(&registry).unwrap_or_default();
    write!(
        existing,
        "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
        payload.display().to_string(),
        manifest_path.display().to_string()
    )
    .expect("write to String cannot fail");
    std::fs::write(&registry, existing).expect("registry");
}

fn desk() -> Desk {
    let apps = tempfile::tempdir().expect("app registry");
    install_app(apps.path(), "so.tilo.calculator", "Calculator", "Utility");
    install_app(apps.path(), NOTES, "Notes", "Office");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", apps.path());
    crate::shell::launcher::invalidate_app_cache();

    let mut tree = WorkspaceTree::bootstrap_demo();
    // The demo seeds invented people; this desk has nobody else anywhere.
    tree.desktop.collaborators.clear();
    for workspace in &mut tree.desktop.workspaces {
        workspace.collaborators.clear();
    }
    let home = tree.desktop.active;
    let research = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.id != home)
        .expect("the demo has a second workspace")
        .id;
    let chrome = ChromeState::new(tree);
    let mut registry = SearchRegistry::new();
    registry.register(Rc::new(PeopleProvider::new()));
    registry.register(Rc::new(WaitingOnYouProvider::new()));
    registry.register(Rc::new(ContinueProvider::without_broker(
        std::sync::Arc::new(crate::shell::search::app_content_index::AppContentIndex::in_memory()),
    )));
    registry.register(Rc::new(ApplicationsProvider::new()));
    let desk = Desk {
        chrome,
        home,
        research,
        registry,
        doc: AutoCommit::new(),
        _apps: apps,
        _env: env,
    };
    desk.with_args("", SearchScope::Workspace, |args| {
        tilo_test_env::wait_until("the installed apps to be indexed", || {
            desk.registry.refresh_indexes(args);
            desk.registry.query_revision(args).contains("ready:")
        });
    });
    desk
}

impl Desk {
    fn with_args<T>(
        &self,
        needle: &str,
        scope: SearchScope,
        run: impl FnOnce(&QueryArgs<'_>) -> T,
    ) -> T {
        let state = self.chrome.borrow();
        run(&QueryArgs {
            needle,
            scope,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &state,
            doc: &self.doc,
            doc_root: &automerge::ROOT,
        })
    }

    fn groups(&self, scope: SearchScope) -> Vec<LauncherGroup> {
        self.with_args("", scope, |args| groups(&self.registry, args))
    }

    fn sections(&self, scope: SearchScope) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for group in self.groups(scope) {
            if out.last() != Some(&group.section.id()) {
                out.push(group.section.id());
            }
        }
        out
    }

    fn rows(&self, scope: SearchScope, section: LauncherSection) -> Vec<SearchResult> {
        self.groups(scope)
            .into_iter()
            .filter(|group| group.section == section)
            .flat_map(|group| group.rows)
            .collect()
    }

    fn room_of(&self, workspace: WorkspaceId) -> String {
        let state = self.chrome.borrow();
        let tree = state.tree();
        tree.desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .expect("the workspace")
            .room()
            .to_string()
    }

    fn first_tab_of(&self, workspace: WorkspaceId) -> (u64, String) {
        let state = self.chrome.borrow();
        let tree = state.tree();
        let tab = tree
            .desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .and_then(|workspace| workspace.strips.first())
            .and_then(|strip| strip.window_groups.first())
            .and_then(|group| group.tabs.first())
            .expect("a window in the workspace");
        let found = (tab.id, tab.title.clone());
        drop(tree);
        found
    }

    /// `name` arrives in `workspace`, in its first window when it has one.
    fn arrive(&self, workspace: WorkspaceId, id: &str, name: &str) {
        let tab = (workspace == self.home).then(|| self.first_tab_of(workspace).0);
        let state = self.chrome.borrow();
        let mut tree = state.tree_mut();
        let workspace = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|candidate| candidate.id == workspace)
            .expect("the workspace");
        workspace.collaborators.push(match tab {
            Some(tab) => Collaborator::with_active_tab(id, name, tab),
            None => Collaborator::new(id, name),
        });
        drop(tree);
    }

    /// A message from `name`, sent from `workspace`.
    fn message(&self, workspace: WorkspaceId, name: &str, text: &str) -> u64 {
        let mut notification = Notification::new(name, text)
            .with_from(name, name.to_lowercase())
            .with_addressed(Addressed::Message);
        notification.source_workspace = Some(workspace.0);
        self.chrome
            .borrow_mut()
            .push_notification_full(notification)
    }

    /// A call to the person themselves, from no workspace, ringing now.
    fn call(&self, name: &str) -> u64 {
        let notification = Notification::new(name, "")
            .with_from(name, name.to_lowercase())
            .with_addressed(Addressed::IncomingCall);
        self.chrome
            .borrow_mut()
            .push_notification_full(notification)
    }

    /// A Notes window in `workspace` shows `title`, as its bar says.
    fn open_note(&self, workspace: WorkspaceId, key: &str, title: &str) {
        let room = self.room_of(workspace);
        recent_places::visit(NOTES, &room, 1, title, "", Some(key));
    }
}

#[test]
#[serial_test::serial(process_env)]
fn each_section_appears_only_with_real_data_and_in_order() {
    let desk = desk();
    assert_eq!(
        desk.sections(SearchScope::Workspace),
        ["apps"],
        "a fresh device has nothing going on, nothing waiting and nothing to continue"
    );

    desk.arrive(desk.home, "morgan", "Morgan");
    assert_eq!(
        desk.sections(SearchScope::Workspace),
        ["happening-now", "apps"]
    );

    desk.message(desk.home, "Alex", "Can you look at the pitch?");
    desk.open_note(desk.home, "pitch", "Pitch");
    assert_eq!(
        desk.sections(SearchScope::Workspace),
        ["happening-now", "waiting-on-you", "continue", "apps"],
        "top to bottom: who is here, what waits, what to continue, the apps"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn the_launcher_is_scoped_to_the_workspace_in_front_until_widened() {
    let desk = desk();
    desk.arrive(desk.home, "morgan", "Morgan");
    desk.arrive(desk.research, "priya", "Priya");
    desk.message(desk.research, "Priya", "Lab results are in");
    desk.open_note(desk.research, "results", "Results");

    let here = desk.rows(SearchScope::Workspace, LauncherSection::HappeningNow);
    assert!(
        here.iter()
            .all(|row| { workspace_of(&row.id).is_none_or(|ws| ws == desk.home) }),
        "only the workspace in front: {:?}",
        here.iter().map(|row| &row.id).collect::<Vec<_>>()
    );
    assert_eq!(
        desk.sections(SearchScope::Workspace),
        ["happening-now", "apps"],
        "Research's message and note are not Home's"
    );

    let everywhere = desk.rows(SearchScope::All, LauncherSection::HappeningNow);
    let workspaces: Vec<WorkspaceId> = everywhere
        .iter()
        .filter(|row| row.id.starts_with("workspace:"))
        .filter_map(|row| workspace_of(&row.id))
        .collect();
    assert_eq!(
        workspaces,
        [desk.home, desk.research],
        "widened, one row per workspace with someone in it, the one in front first"
    );
    assert_eq!(
        desk.sections(SearchScope::All),
        ["happening-now", "waiting-on-you", "continue", "apps"]
    );

    // A call to the person, from no workspace, waits on them here too.
    desk.call("Bo");
    let waiting = desk.rows(SearchScope::Workspace, LauncherSection::WaitingOnYou);
    assert_eq!(
        waiting
            .iter()
            .filter_map(|row| row.facts.first())
            .collect::<Vec<_>>(),
        ["Bo"],
        "the call, and not Research's message"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn every_launcher_row_says_why_it_is_there() {
    let desk = desk();
    desk.arrive(desk.home, "morgan", "Morgan");
    desk.arrive(desk.research, "priya", "Priya");
    desk.message(desk.home, "Alex", "Can you look at the pitch?");
    desk.open_note(desk.home, "pitch", "Pitch");
    let (_, window) = desk.first_tab_of(desk.home);

    let groups = desk.groups(SearchScope::All);
    for group in &groups {
        for row in &group.rows {
            let reason = if row.facts.is_empty() {
                row.subtitle.clone().into_iter().collect()
            } else {
                row.facts.clone()
            };
            assert!(
                reason.iter().any(|line| !line.trim().is_empty()),
                "{:?} row {:?} says nothing about why it is there",
                group.label,
                row.title
            );
        }
    }
    let happening = desk.rows(SearchScope::All, LauncherSection::HappeningNow);
    let home = happening
        .iter()
        .find(|row| row.id == format!("workspace:{}", desk.home.0))
        .expect("Home's row");
    assert_eq!(home.subtitle.as_deref(), Some("Morgan is here"));
    let in_window = happening
        .iter()
        .find(|row| row.title == window)
        .expect("the window Morgan is in");
    assert_eq!(in_window.subtitle.as_deref(), Some("With Morgan"));
    let waiting = desk.rows(SearchScope::All, LauncherSection::WaitingOnYou);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].facts.first().map(String::as_str), Some("Alex"));
    let continuing = desk.rows(SearchScope::All, LauncherSection::Continue);
    assert_eq!(continuing.len(), 1);
    assert!(
        continuing[0]
            .facts
            .iter()
            .any(|fact| fact.starts_with("Opened ")),
        "{:?}",
        continuing[0].facts
    );
}

#[test]
#[serial_test::serial(process_env)]
fn apps_lists_the_most_used_then_tilos_starting_points_never_by_category() {
    let desk = desk();
    let titles = |desk: &Desk| -> Vec<String> {
        desk.rows(SearchScope::Workspace, LauncherSection::Apps)
            .into_iter()
            .map(|row| row.title)
            .collect()
    };
    assert_eq!(
        desk.groups(SearchScope::Workspace)
            .iter()
            .filter(|group| group.section == LauncherSection::Apps)
            .count(),
        1,
        "one Apps section, no group per category"
    );
    assert_eq!(
        titles(&desk),
        ["Notes", "Calculator"],
        "nothing used yet: Tilo's starting points, in their order"
    );
    let calculator = desk
        .rows(SearchScope::Workspace, LauncherSection::Apps)
        .into_iter()
        .find(|row| row.title == "Calculator")
        .expect("Calculator");
    usage::record(&calculator.provider_id, &calculator.id);
    assert_eq!(
        titles(&desk),
        ["Calculator", "Notes"],
        "the most used lead, one row per app"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn a_typed_query_is_not_the_launcher() {
    let desk = desk();
    desk.arrive(desk.home, "morgan", "Morgan");
    let typed = desk.with_args("calc", SearchScope::Workspace, |args| {
        desk.registry.query_grouped(args, 0)
    });
    assert!(
        typed
            .iter()
            .flat_map(|group| &group.results)
            .all(|row| !row.id.starts_with("workspace:")),
        "typing turns the launcher into results"
    );
    let cleared = desk.with_args("", SearchScope::Workspace, |args| {
        desk.registry.query_grouped(args, 0)
    });
    assert_eq!(
        cleared.first().map(|group| &group.kind),
        Some(&ResourceKind::Custom(LauncherSection::HappeningNow.label())),
        "clearing the query brings the launcher back"
    );
}
