//! A branch takes its source's machine inside the branch, and a deletion
//! takes the workspace's machine before the workspace: what runs the machines
//! is asked in that order, and a machine that does not come fails the whole.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the host a test binds is a shared handle it keeps until the test ends; dropping it earlier closes the host and releases no lock"
)]

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
        let relay = test_host(dir.path(), &key, &cert).await;
        let workspaces = relay.workspaces();
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
        relay.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_source_without_a_machine_branches_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let relay = test_host(dir.path(), &key, &cert).await;
        let workspaces = relay.workspaces();
        let source = source(workspaces).await;
        machines(workspaces, Ok(false), Ok(()));

        workspaces
            .branch_own(&source, BRANCH, "Task", PERSON)
            .await
            .map_err(|not| not.refusal())
            .expect("nothing to copy is no failure");
        assert!(workspaces.own().unwrap().is_of(&PERSON, BRANCH));
        relay.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_branch_whose_machine_did_not_come_is_removed_saying_so() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let relay = test_host(dir.path(), &key, &cert).await;
        let workspaces = relay.workspaces();
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
            tilo_relay_session::workspaces::branch_not_made_part(&refused),
            Some("its Linux machine"),
            "the refusal names the machine as what did not come: {refused}"
        );
        assert!(refused.contains("no room"), "and says why: {refused}");
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
        relay.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_that_runs_no_machines_branches_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let relay = test_host(dir.path(), &key, &cert).await;
        let workspaces = relay.workspaces();
        let source = source(workspaces).await;

        workspaces
            .branch_own(&source, BRANCH, "Task", PERSON)
            .await
            .map_err(|not| not.refusal())
            .unwrap();
        assert!(workspaces.own().unwrap().is_of(&PERSON, BRANCH));
        relay.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_is_deleted_after_its_machine_and_stays_when_that_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
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
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_runs_no_machines_deletes_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let gone = workspaces.create("Gone").await.unwrap().workspace;
    workspaces.delete(&gone).await.unwrap();
    assert!(workspaces.summary(&gone).await.is_err());
    relay.shutdown().await;
}
