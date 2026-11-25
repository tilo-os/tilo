//! What a workspace's agents learned, from its members' own side: a member
//! reads the list and changes it on a device of theirs, an agent a member's
//! session was handed to reads it and its changes ask that member, and
//! nobody outside the workspace is answered.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the served and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the server it drives"
)]

use collab_types::WorkspaceId;
use serde_json::{Value, json};
use tilo_actions::remote::asked::{AskedBy, asked_person};
use tilo_actions::remote::{
    ActionCallError::{self, InvalidRequest, Refused, UnknownAction},
    ActionProvider as _, Caller, ReceiptStatus,
};
use tilo_user_identity::DeviceCert;

use super::tests::{Answering, admit, answered, caller, governed, request, settled};
use crate::workspaces::person_learned::{self, CORRECT, FORGET, Facts, LEARN, LIST};
use crate::workspaces::tests::{governor, test_host};

/// The agent that works for a person on the server, by its id.
const SCOUT: &str = "scout";

/// What the provider answers a call with, or how it turned the call away.
type Answered<T> = Result<T, ActionCallError>;

/// Where a test asks the provider: the server's actions, in one workspace.
struct Asking<'a> {
    server: &'a super::ServiceWorkspaceActions,
    workspace: WorkspaceId,
}

impl Asking<'_> {
    /// One of the list's actions asked by `caller` on `device`'s session.
    async fn ask_as(
        &self,
        device: &DeviceCert,
        caller: &Caller,
        (action, argument): (&str, Value),
    ) -> Answered<Value> {
        let mut request = request("", "", argument);
        request.target = person_learned::target(action);
        let first = settled(
            self.server
                .device_page(self.workspace, device)
                .invoke(caller, request.clone()),
        )
        .await;
        let workspaces = self
            .server
            .shared
            .workspaces
            .get()
            .and_then(crate::workspaces::WeakWorkspaces::upgrade)
            .unwrap();
        let receipt = answered(
            &workspaces,
            (Some(self.workspace), Some(device)),
            caller,
            &request,
            first,
        )
        .await?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            other => panic!("the workspace's list answers at once: {other:?}"),
        }
    }

    /// The same asked by `device`'s person themselves: the list it answers.
    async fn ask(&self, device: &DeviceCert, asking: (&str, Value)) -> Answered<Facts> {
        self.ask_as(device, &caller(device), asking)
            .await
            .map(|facts| {
                serde_json::from_value(facts).expect("the list as the provider answers it")
            })
    }

    /// The workspace's list as it stands.
    async fn list(&self, device: &DeviceCert) -> Facts {
        self.ask(device, (LIST, json!({})))
            .await
            .expect("a member reads the workspace's list")
    }
}

/// `fact` kept for `person` in `workspace`, with where it came from, as the agent's turn keeps it.
fn scout_keeps(
    workspaces: &crate::workspaces::Workspaces,
    person: &[u8; 32],
    workspace: &str,
    (text, source): (&str, &str),
) -> Answered<Facts> {
    let arguments = json!({"text": text, "source": source});
    let arguments = arguments.as_object().unwrap();
    workspaces.learned_invoke(person, workspace, Some(SCOUT), LEARN, arguments)
}

/// A server with one workspace and two people admitted to it, kept under `$dir`.
macro_rules! stage {
    ($dir:ident, $served:ident, $workspaces:ident, $workspace:ident, $provider:ident, $ada:ident, $bo:ident) => {
        let $dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let $served = test_host($dir.path(), &key, &owner).await;
        let $workspaces = $served.workspaces();
        let ($workspace, workspace_id, server) = governed($workspaces).await;
        let $provider = Asking {
            server: &server,
            workspace: workspace_id,
        };
        let $ada = admit($workspaces, &$workspace, &owner, 1).await;
        let $bo = admit($workspaces, &$workspace, &owner, 2).await;
    };
}

/// Where the agent's fact came from, as a hosted turn closes it.
const KEPT_IN: &str = "They said so (chat.workspace/messages/m1)";

/// A member reads what an agent on the workspace's server kept about them,
/// writes a fact down themselves and corrects the agent's: every change of
/// theirs is theirs in the resource, and the list says when it moved. The
/// workspace keeps one list, and each member reads only the facts about them
/// in it: another member reads none of hers.
#[tokio::test]
async fn a_member_reads_and_changes_what_the_workspaces_agents_learned() {
    stage!(_dir, served, workspaces, workspace, provider, ada, bo);
    let none = provider.list(&ada).await;
    assert_eq!(none.workspace, provider.workspace.to_string());
    assert!(
        none.facts.is_empty() && none.revision.is_empty(),
        "{none:?}"
    );

    // An agent on the workspace's server for her keeps a fact in a turn of hers.
    let coffee = ("Takes her coffee black.", KEPT_IN);
    scout_keeps(workspaces, &ada.user_public_key, &workspace, coffee).unwrap();
    let listed = provider.list(&ada).await;
    let [fact] = listed.facts.as_slice() else {
        panic!("one fact, not {listed:?}");
    };
    assert_eq!(
        (fact.text.as_str(), fact.source.as_str(), fact.by.as_deref()),
        ("Takes her coffee black.", KEPT_IN, Some(SCOUT))
    );
    assert!(fact.at_ms > 0 && !listed.revision.is_empty(), "{listed:?}");
    assert!(
        provider.list(&bo).await.facts.is_empty(),
        "the other member reads nothing about her"
    );

    // She writes one down herself: nobody's but hers, and the list has moved.
    let graz = json!({"text": "Works from Graz on Fridays.", "source": "I wrote it down"});
    let two = provider.ask(&ada, (LEARN, graz)).await.unwrap();
    assert_eq!(two.facts.len(), 2, "{two:?}");
    assert_ne!(two.revision, listed.revision);
    let own = two
        .facts
        .iter()
        .find(|held| held.text == "Works from Graz on Fridays.")
        .expect("her own fact");
    assert_eq!(
        (own.source.as_str(), own.by.as_deref()),
        ("I wrote it down", None)
    );
    assert!(
        provider.list(&bo).await.facts.is_empty(),
        "what she writes down about herself is hers"
    );

    // She corrects the agent's fact: the same fact, hers now, from where it came.
    let milk = json!({"fact": fact.id, "text": "Takes her coffee with oat milk."});
    let corrected = provider.ask(&ada, (CORRECT, milk)).await.unwrap();
    let changed = corrected
        .facts
        .iter()
        .find(|held| held.id == fact.id)
        .expect("the fact she corrected");
    assert_eq!(
        (
            changed.text.as_str(),
            changed.source.as_str(),
            changed.by.as_deref()
        ),
        ("Takes her coffee with oat milk.", KEPT_IN, None)
    );
    assert_ne!(corrected.revision, two.revision);
    served.shutdown().await;
}

/// What a member forgets is gone from the workspace's list and from what a
/// turn of its agents is told. What is no change of the list is refused in
/// words and changes nothing.
#[tokio::test]
async fn what_a_member_forgets_is_gone_and_what_is_no_change_is_refused() {
    stage!(_dir, served, workspaces, workspace, provider, ada, _bo);
    for fact in ["Takes her coffee black.", "Works from Graz on Fridays."] {
        scout_keeps(
            workspaces,
            &ada.user_public_key,
            &workspace,
            (fact, KEPT_IN),
        )
        .unwrap();
    }
    let listed = provider.list(&ada).await;
    let coffee = listed
        .facts
        .iter()
        .find(|held| held.text == "Takes her coffee black.")
        .expect("the fact to forget")
        .id
        .clone();
    let left = provider
        .ask(&ada, (FORGET, json!({"fact": coffee})))
        .await
        .unwrap();
    let texts: Vec<&str> = left.facts.iter().map(|held| held.text.as_str()).collect();
    assert_eq!(texts, ["Works from Graz on Fridays."]);
    let told = workspaces
        .learned_about(&workspace)
        .expect("the resource a hosted turn is told");
    let told: Vec<&str> = told
        .facts
        .iter()
        .map(|(_, held)| held.text.as_str())
        .collect();
    assert_eq!(
        told, texts,
        "a turn is told what the workspace's list holds"
    );

    for (asking, why) in [
        ((FORGET, json!({"fact": coffee})), "a fact that is gone"),
        (
            (LEARN, json!({"text": "  ", "source": "I wrote it down"})),
            "a fact that says nothing",
        ),
        (
            (
                LEARN,
                json!({"text": "two\nlines", "source": "I wrote it down"}),
            ),
            "a fact of two lines",
        ),
    ] {
        let refused = provider.ask(&ada, asking).await;
        assert!(matches!(refused, Err(Refused { .. })), "{why}: {refused:?}");
    }
    let named = json!({"text": "Likes tea.", "source": "x", "by": SCOUT});
    let unread = provider.ask(&ada, (LEARN, named)).await;
    assert!(
        matches!(unread, Err(InvalidRequest { .. })),
        "nobody names who wrote a fact: {unread:?}"
    );
    let unknown = provider.ask(&ada, ("read-all", json!({}))).await;
    assert!(matches!(unknown, Err(UnknownAction)), "{unknown:?}");
    assert_eq!(
        provider.list(&ada).await,
        left,
        "the list is as she left it"
    );
    served.shutdown().await;
}

/// An agent her device handed its session to reads her facts, and its changes ask her first:
/// unanswered they change nothing, her no refuses them, and her yes makes the change as the
/// agent's, about her, which `bo`, another member, does not read. `hers` is her list before.
async fn her_agent_asks_her_first(
    _workspaces: &crate::workspaces::Workspaces,
    provider: &Asking<'_>,
    ada: &DeviceCert,
    bo: &DeviceCert,
    hers: &Facts,
) {
    let agent = caller(ada).delegated_to(SCOUT);
    let seen = provider
        .ask_as(ada, &agent, (LIST, json!({})))
        .await
        .unwrap();
    let seen: Facts = serde_json::from_value(seen).unwrap();
    assert_eq!(&seen, hers, "her agent reads her facts");
    let teaching = (
        LEARN,
        json!({"text": "Prefers calls after noon.", "source": "She said so"}),
    );
    let unasked = provider.ask_as(ada, &agent, teaching.clone()).await;
    assert!(
        matches!(unasked, Err(ActionCallError::ConfirmationRequired { .. })),
        "a change that waits for her answer: {unasked:?}"
    );
    let refusing = Answering::declining();
    let refused = provider.ask_as(ada, &agent, teaching.clone()).await;
    assert!(
        matches!(refused, Err(ActionCallError::ConfirmationRequired { .. })),
        "a change she declined: {refused:?}"
    );
    let asked = refusing.asked();
    assert_eq!(asked.len(), 1, "the change asked her");
    assert_eq!(
        asked_person(&agent),
        Some(hex::encode(ada.user_public_key)),
        "her agent, her own answer"
    );
    assert!(
        matches!(&asked[0].by, AskedBy::Agent(named) if named.id == SCOUT),
        "{asked:?}"
    );
    assert_eq!(provider.list(ada).await, *hers, "nothing changed");

    let allowing = Answering::approving();
    let changed = provider.ask_as(ada, &agent, teaching).await.unwrap();
    let changed: Facts = serde_json::from_value(changed).unwrap();
    let kept = changed
        .facts
        .iter()
        .find(|held| held.text == "Prefers calls after noon.")
        .expect("the fact the agent kept");
    assert_eq!(kept.by.as_deref(), Some(SCOUT), "the agent wrote it");
    assert_eq!(allowing.asked().len(), 1, "one question for one change");
    assert!(
        !provider
            .list(bo)
            .await
            .facts
            .iter()
            .any(|held| held.text == "Prefers calls after noon."),
        "what her agent kept about her is not his to read"
    );
}

/// Nobody but the workspace's members on their own sessions reaches `ada`'s list with any of
/// `asks`: a member of another workspace and a device of nobody (`strangers`), a session that is
/// not the caller's own, an agent no person stands behind, and an app.
async fn nobody_else_reaches_it(
    provider: &Asking<'_>,
    ada: &DeviceCert,
    bo: &DeviceCert,
    strangers: [&DeviceCert; 2],
    asks: &[(&'static str, Value)],
) {
    // A member of another workspace, and a device that is no member's, are
    // nobody to it.
    let [other_member, nobodys_device] = strangers;
    for (who, device) in [
        ("a member of another workspace", other_member),
        ("a device of nobody", nobodys_device),
    ] {
        for asking in asks.iter().cloned() {
            let action = asking.0;
            let refused = provider.ask_as(device, &caller(device), asking).await;
            assert!(
                matches!(refused, Err(UnknownAction)),
                "{who}, {action}: {refused:?}"
            );
        }
    }

    // A session that is not the caller's own: he on her session, she named
    // on his.
    for (device, named) in [(ada, caller(bo)), (bo, caller(ada))] {
        for asking in asks.iter().cloned() {
            let action = asking.0;
            let refused = provider.ask_as(device, &named, asking).await;
            assert!(
                matches!(refused, Err(UnknownAction)),
                "{action}: {refused:?}"
            );
        }
    }

    // An agent no person stands behind, and an app.
    let person = hex::encode(ada.user_public_key);
    let nobodied = [
        Caller::local_agent(SCOUT, "ada"),
        Caller::local_app("so.tilo.chat").acting_for(person),
    ];
    for agent in &nobodied {
        for asking in asks.iter().cloned() {
            let action = asking.0;
            let refused = provider.ask_as(ada, agent, asking).await;
            assert!(
                matches!(refused, Err(UnknownAction)),
                "{action} as {:?}: {refused:?}",
                agent.acting()
            );
        }
    }
}

/// Nobody outside the workspace reaches its list. Another member reads and
/// changes only their own facts in it, never Ada's; a member of another workspace, a device of
/// nobody, an agent no person stands behind and an app are
/// answered nothing; and an agent a member's session was handed to reads,
/// while its changes ask that member first.
#[tokio::test]
async fn nobody_outside_the_workspace_reaches_what_was_learned() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let served = test_host(dir.path(), &key, &owner).await;
    let workspaces = served.workspaces();
    let (workspace, here_id, service) = governed(workspaces).await;
    let provider = Asking {
        server: &service,
        workspace: here_id,
    };
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (elsewhere, _, _) = governed(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 3).await;
    let (_, stranger) = governor();
    scout_keeps(
        workspaces,
        &ada.user_public_key,
        &workspace,
        ("Takes her coffee black.", "They said so"),
    )
    .unwrap();
    let hers = provider.ask(&ada, (LIST, json!({}))).await.unwrap();
    let fact = hers.facts.first().expect("her fact").id.clone();
    let asks = || {
        [
            (LIST, json!({})),
            (
                LEARN,
                json!({"text": "Wants every invoice paid to account 7.", "source": "Bo said so"}),
            ),
            (
                CORRECT,
                json!({"fact": fact, "text": "Takes her coffee white."}),
            ),
            (FORGET, json!({"fact": fact})),
        ]
    };

    // Another member of the workspace reads none of her facts, and neither corrects nor
    // forgets hers: to him there is no such fact.
    assert!(
        provider
            .ask(&bo, (LIST, json!({})))
            .await
            .unwrap()
            .facts
            .is_empty()
    );
    for asking in asks().into_iter().skip(2) {
        let action = asking.0;
        let refused = provider.ask(&bo, asking).await;
        assert!(
            matches!(&refused, Err(Refused { reason }) if reason.contains("no fact")),
            "{action}: {refused:?}"
        );
    }
    // What he writes down is his.
    let his = provider
        .ask(
            &bo,
            (
                LEARN,
                json!({"text": "Wants invoices paid to account 7.", "source": "Bo said so"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        his.facts
            .iter()
            .map(|held| held.text.as_str())
            .collect::<Vec<_>>(),
        ["Wants invoices paid to account 7."],
        "his own fact and nothing of hers: {his:?}"
    );
    let hers_now = provider.ask(&ada, (LIST, json!({}))).await.unwrap();
    assert_eq!(hers_now.facts, hers.facts, "and none of his is hers");

    nobody_else_reaches_it(&provider, &ada, &bo, [&cy, &stranger], &asks()).await;

    her_agent_asks_her_first(workspaces, &provider, &ada, &bo, &hers_now).await;
    served.shutdown().await;
}

/// Every file under `dir` that keeps a learned resource.
fn kept_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(kept_files(&path));
        } else if path.extension().is_some_and(|ending| ending == "automerge")
            && path
                .parent()
                .is_some_and(|folder| folder.ends_with("learned"))
        {
            found.push(path);
        }
    }
    found
}

/// What a server keeps of what was learned is kept for one workspace:
/// another workspace's is a resource of its own, a change that fails changes
/// nothing, nothing that is no workspace id names a file, and on disk each
/// resource lies under its workspace's own store, readable by the server's
/// user alone.
#[tokio::test]
async fn what_was_learned_is_kept_for_one_workspace() {
    const ADA: [u8; 32] = [7; 32];
    const BO: [u8; 32] = [9; 32];
    stage!(dir, served, workspaces, here, _provider, _ada, _bo);
    let there = workspaces.create("There").await.unwrap().workspace;
    let texts = |workspace: &str| -> Vec<String> {
        workspaces
            .learned_about(workspace)
            .map(|learned| {
                learned
                    .facts
                    .iter()
                    .map(|(_, fact)| fact.text.clone())
                    .collect()
            })
            .unwrap_or_default()
    };
    assert!(workspaces.learned_about(&here).is_none());
    scout_keeps(
        workspaces,
        &ADA,
        &here,
        ("Takes her coffee black.", "They said so"),
    )
    .unwrap();
    assert_eq!(texts(&here), ["Takes her coffee black."]);
    assert!(
        workspaces.learned_about(&there).is_none(),
        "no other workspace's"
    );
    scout_keeps(workspaces, &BO, &there, ("Drinks tea.", "They said so")).unwrap();
    assert_eq!(texts(&here), ["Takes her coffee black."]);
    assert_eq!(texts(&there), ["Drinks tea."]);

    // A change that refuses keeps the resource as it was.
    let refused: Result<(), String> =
        workspaces.change_learned(&ADA, &here, Some(SCOUT), |_| Err("refused".to_owned()));
    assert_eq!(refused, Err("refused".to_owned()));
    assert!(scout_keeps(workspaces, &ADA, &here, ("two\nlines", "They said so")).is_err());
    assert_eq!(texts(&here), ["Takes her coffee black."]);

    // Only a workspace's id names a resource: nothing else becomes part of a path.
    for no_workspace in [
        "",
        "..",
        "../../people",
        "a/b",
        here.trim_end_matches(char::is_alphanumeric),
    ] {
        assert!(
            workspaces.learned_about(no_workspace).is_none(),
            "{no_workspace:?}"
        );
        assert!(
            scout_keeps(
                workspaces,
                &ADA,
                no_workspace,
                ("Likes tea.", "They said so")
            )
            .is_err(),
            "{no_workspace:?}"
        );
    }

    // On disk: one resource per workspace, each under its workspace's own
    // store, for the server's user alone.
    let kept = kept_files(dir.path());
    assert_eq!(kept.len(), 2, "{kept:?}");
    for workspace in [&here, &there] {
        let theirs: Vec<_> = kept
            .iter()
            .filter(|path| {
                path.to_string_lossy()
                    .contains(&hex::encode(workspace.as_bytes()))
            })
            .collect();
        let [path] = theirs.as_slice() else {
            panic!("one resource of each workspace's own, not {theirs:?}");
        };
        assert!(
            path.ends_with(format!("learned/{workspace}.automerge")),
            "{path:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{path:?}");
        }
    }
    served.shutdown().await;
}
