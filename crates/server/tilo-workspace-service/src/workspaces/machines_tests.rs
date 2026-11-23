//! A branch takes its source's machine inside the branch, and a deletion
//! takes the workspace's machine before the workspace: what runs the machines
//! is asked in that order, and a machine that does not come fails the whole.

use std::sync::{Arc, Mutex};

use super::WorkspaceMachines;
use crate::workspaces::Workspaces;
use crate::workspaces::tests::{governor, test_host};

/// What a test's machines were asked, in order, and whether the workspace
/// asked about was still governed when they were.
type Asked = Arc<Mutex<Vec<(String, bool)>>>;

/// Machines that answer as a test sets, noting each question.
struct Machines {
    workspaces: Workspaces,
    asked: Asked,
    /// What a branch answers: made, no machine at the source, or why not.
    branch: Result<bool, String>,
    /// What a deletion answers.
    delete: Result<(), String>,
}

impl Machines {
    fn note(&self, what: String, workspace: &str) {
        let workspaces = self.workspaces.clone();
        let workspace = workspace.to_owned();
        // Asked off the runtime's threads, as the host asks: whether the
        // workspace is still there is read the way any caller reads it.
        let held = tokio::runtime::Handle::current()
            .block_on(async move { workspaces.summary(&workspace).await.is_ok() });
        self.asked.lock().unwrap().push((what, held));
    }
}

impl WorkspaceMachines for Machines {
    fn branch(&self, source: &str, branch: &str) -> Result<bool, String> {
        self.note(format!("branch {source} into {branch}"), branch);
        self.branch.clone()
    }

    fn delete(&self, workspace: &str) -> Result<(), String> {
        self.note(format!("delete {workspace}"), workspace);
        self.delete.clone()
    }

    fn drain(&self) -> Result<(), String> {
        Ok(())
    }
}

fn machines(
    workspaces: &Workspaces,
    branch: Result<bool, String>,
    delete: Result<(), String>,
) -> Asked {
    let asked = Asked::default();
    workspaces.machines(Arc::new(Machines {
        workspaces: workspaces.clone(),
        asked: Arc::clone(&asked),
        branch,
        delete,
    }));
    asked
}

fn asked(asked: &Asked) -> Vec<(String, bool)> {
    asked.lock().unwrap().clone()
}

#[cfg(feature = "agents")]
mod branching {
    use super::*;
    use crate::workspaces::OwnWorkspaceOffer;

    const PERSON: [u8; 32] = [5; 32];
    const BRANCH: &str = "6f2b8a51-6d0f-4d4e-9d6c-5d3b3f1f2a10";

    /// A host that keeps workspaces of people's own, and a source of the
    /// person's to branch.
    async fn source(workspaces: &Workspaces) -> String {
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap();
        workspaces.create("Template").await.unwrap().workspace
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_branch_takes_its_sources_machine_before_it_is_the_persons() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let source = source(workspaces).await;
        let noted = machines(workspaces, Ok(true), Ok(()));

        workspaces
            .branch_own(&source, BRANCH, "Task", PERSON)
            .await
            .map_err(|not| not.refusal())
            .unwrap();
        assert_eq!(
            asked(&noted),
            [(format!("branch {source} into {BRANCH}"), true)],
            "the machine was copied once, into the branch while it was there to hold"
        );
        assert!(workspaces.own().unwrap().is_of(&PERSON, BRANCH));
        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_source_without_a_machine_branches_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let source = source(workspaces).await;
        machines(workspaces, Ok(false), Ok(()));

        workspaces
            .branch_own(&source, BRANCH, "Task", PERSON)
            .await
            .map_err(|not| not.refusal())
            .expect("nothing to copy is no failure");
        assert!(workspaces.own().unwrap().is_of(&PERSON, BRANCH));
        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_branch_whose_machine_did_not_come_is_removed_saying_so() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let source = source(workspaces).await;
        let noted = machines(
            workspaces,
            Err("tilo-vm-host-0 refused it: no room".into()),
            Ok(()),
        );

        let Err(not) = workspaces.branch_own(&source, BRANCH, "Task", PERSON).await else {
            panic!("a branch whose machine did not come is no branch");
        };
        let refused = not.refusal();
        assert_eq!(
            tilo_server_session::workspaces::AdminRefusal::from_reason(&refused),
            Some(
                tilo_server_session::workspaces::AdminRefusal::BranchNotMade {
                    part: "its Linux machine".into(),
                }
            ),
            "the refusal names the machine as what did not come: {refused}"
        );
        assert_eq!(
            asked(&noted),
            [
                (format!("branch {source} into {BRANCH}"), true),
                (format!("delete {BRANCH}"), true),
            ],
            "the branch was undone as any workspace is deleted: its machine first, while the \
             branch was still there"
        );
        assert!(
            workspaces.summary(BRANCH).await.is_err(),
            "the branch is gone"
        );
        assert!(!workspaces.own().unwrap().is_of(&PERSON, BRANCH));
        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_branch_whose_machine_did_not_come_is_made_again_under_its_identity() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let source = source(workspaces).await;
        let refusing = machines(workspaces, Err("no room".into()), Ok(()));
        assert!(
            workspaces
                .branch_own(&source, BRANCH, "Task", PERSON)
                .await
                .is_err()
        );
        assert_eq!(
            asked(&refusing),
            [
                (format!("branch {source} into {BRANCH}"), true),
                (format!("delete {BRANCH}"), true),
            ],
            "its machine went first, while the branch was there"
        );
        let id = tilo_server_protocol::WorkspaceId::parse(BRANCH).unwrap();
        assert!(!workspaces.deleted(id), "discarded, not deleted");
        assert!(
            !workspaces.unfinished_branch(BRANCH),
            "nothing of it is left"
        );

        // Asked again under the same identity, once a machine can come, it is made.
        let noted = machines(workspaces, Ok(true), Ok(()));
        workspaces
            .branch_own(&source, BRANCH, "Task", PERSON)
            .await
            .map_err(|not| not.refusal())
            .unwrap();
        assert_eq!(
            asked(&noted),
            [(format!("branch {source} into {BRANCH}"), true)]
        );
        assert!(workspaces.own().unwrap().is_of(&PERSON, BRANCH));
        assert!(!workspaces.unfinished_branch(BRANCH));
        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_branch_a_stop_cut_short_opens_as_nothing_and_is_made_again() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let source = source(workspaces).await;
        // A fill the host stopped in: the branch made and even recorded as the
        // person's, its marker not yet gone.
        workspaces.begin_branch(BRANCH).await.unwrap();
        workspaces
            .create_workspace("Task", Some(BRANCH))
            .await
            .unwrap();
        workspaces.own().unwrap().record(&PERSON, BRANCH).unwrap();
        server.shutdown().await;

        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap();
        assert!(
            workspaces.summary(BRANCH).await.is_err(),
            "the host governs none of it"
        );
        assert!(
            !workspaces
                .list()
                .await
                .iter()
                .any(|one| one.workspace == BRANCH),
            "nor lists it"
        );
        assert!(workspaces.unfinished_branch(BRANCH));
        let noted = machines(workspaces, Ok(true), Ok(()));
        workspaces
            .branch_own(&source, BRANCH, "Task", PERSON)
            .await
            .map_err(|not| not.refusal())
            .unwrap();
        assert_eq!(
            asked(&noted),
            [
                (format!("delete {BRANCH}"), false),
                (format!("branch {source} into {BRANCH}"), true),
            ],
            "what the stop left of its machine went before the branch was made again"
        );
        assert!(workspaces.own().unwrap().is_of(&PERSON, BRANCH));
        assert!(workspaces.summary(BRANCH).await.is_ok());
        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_that_runs_no_machines_branches_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let source = source(workspaces).await;

        workspaces
            .branch_own(&source, BRANCH, "Task", PERSON)
            .await
            .map_err(|not| not.refusal())
            .unwrap();
        assert!(workspaces.own().unwrap().is_of(&PERSON, BRANCH));
        server.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_is_deleted_after_its_machine_and_stays_when_that_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let kept = workspaces.create("Kept").await.unwrap().workspace;
    let gone = workspaces.create("Gone").await.unwrap().workspace;

    let noted = machines(
        workspaces,
        Ok(true),
        Err("tilo-vm-host-0 does not answer".into()),
    );
    let refused = workspaces.delete(&kept).await.unwrap_err();
    assert!(
        refused.contains("Linux machine was not deleted") && refused.ends_with("does not answer"),
        "the deletion says the machine stopped it: {refused}"
    );
    assert!(
        workspaces.summary(&kept).await.is_ok(),
        "the workspace stays, to be deleted again"
    );
    assert!(!workspaces.deleted(workspaces.summary(&kept).await.unwrap().workspace_id));
    assert_eq!(asked(&noted), [(format!("delete {kept}"), true)]);

    let noted = machines(workspaces, Ok(true), Ok(()));
    workspaces.delete(&gone).await.unwrap();
    assert_eq!(
        asked(&noted),
        [(format!("delete {gone}"), true)],
        "its machine went first, while the workspace was still there for its host to hold"
    );
    assert!(workspaces.summary(&gone).await.is_err());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_its_owner_deleted_stays_refused_under_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let made = workspaces.create("Gone").await.unwrap();
    workspaces.delete(&made.workspace).await.unwrap();
    assert!(workspaces.deleted(made.workspace_id));
    let again = workspaces
        .create_workspace("Gone", Some(&made.workspace))
        .await
        .unwrap_err();
    assert!(again.contains("deleted here"), "{again}");
    server.shutdown().await;

    // Also after the host opens again.
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    assert!(workspaces.deleted(made.workspace_id));
    assert!(
        workspaces
            .create_workspace("Gone", Some(&made.workspace))
            .await
            .is_err()
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_runs_no_machines_deletes_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let gone = workspaces.create("Gone").await.unwrap().workspace;
    workspaces.delete(&gone).await.unwrap();
    assert!(workspaces.summary(&gone).await.is_err());
    server.shutdown().await;
}
