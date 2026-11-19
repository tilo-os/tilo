//! What a server's agents learned about a person, from the person's own side: they read the list
//! and change it on a device of theirs, nobody else is answered, and each person's list in each
//! workspace is a resource of its own.
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
use tilo_actions::remote::{
    ActionCallError::{self, InvalidRequest, Refused, UnknownAction},
    ActionProvider as _, Caller, ReceiptStatus,
};
use tilo_user_identity::DeviceCert;

use super::tests::{admit, caller, governed, request, settled};
use crate::workspaces::person_learned::{
    self, CONFIRM, CORRECT, FORGET, FactAuthority, FactBoundaryWake, FactCorrection, FactUpdate,
    FactView, Facts, IsoDate, LEARN, LIST, WindowBoundary,
};
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
        let receipt = settled(
            self.server
                .in_workspace_as(self.workspace, device)
                .invoke(caller, request),
        )
        .await?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            other => panic!("a person's own list answers at once: {other:?}"),
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

    /// The person's list as it stands.
    async fn list(&self, device: &DeviceCert) -> Facts {
        self.ask(device, (LIST, json!({})))
            .await
            .expect("a person reads their own list")
    }
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

/// The person reads what an agent on a workspace's server for them kept, writes a fact down themselves
/// and corrects the agent's: every change of theirs is theirs in the resource, and the list says
/// when it moved.
#[tokio::test]
async fn a_person_reads_and_changes_what_the_servers_agents_learned_about_them() {
    stage!(_dir, served, workspaces, workspace, provider, ada, _bo);
    let none = provider.list(&ada).await;
    assert_eq!(none.workspace, provider.workspace.to_string());
    assert!(
        none.facts.is_empty() && none.revision.is_empty(),
        "{none:?}"
    );

    // An agent on a workspace's server for her keeps a fact in a turn of hers.
    let coffee = ("Takes her coffee black.", KEPT_IN);
    workspaces
        .keep_learned_fact(&ada.user_public_key, &workspace, SCOUT, coffee)
        .unwrap();
    let listed = provider.list(&ada).await;
    let [fact] = listed.facts.as_slice() else {
        panic!("one fact, not {listed:?}");
    };
    assert_eq!(
        (fact.text.as_str(), fact.source.as_str(), fact.by.as_deref()),
        ("Takes her coffee black.", KEPT_IN, Some(SCOUT))
    );
    assert!(fact.at_ms > 0 && !listed.revision.is_empty(), "{listed:?}");

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

/// What she forgets is gone from her list and from what a turn of hers is told. What is no
/// change of the list is refused in words and changes nothing.
#[tokio::test]
async fn what_a_person_forgets_is_gone_and_what_is_no_change_is_refused() {
    stage!(_dir, served, workspaces, workspace, provider, ada, _bo);
    for fact in ["Takes her coffee black.", "Works from Graz on Fridays."] {
        workspaces
            .keep_learned_fact(&ada.user_public_key, &workspace, SCOUT, (fact, KEPT_IN))
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
        .learned_about(&ada.user_public_key, &workspace)
        .expect("the resource a hosted turn is told");
    let told: Vec<&str> = told
        .facts
        .iter()
        .map(|(_, held)| held.text.as_str())
        .collect();
    assert_eq!(told, texts, "a turn of hers is told what her list holds");

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
        "her list is as she left it"
    );
    served.shutdown().await;
}

/// Nobody but the person reaches their list. Another member reads and changes only their own,
/// a session that is not the caller's own is answered nothing, and no agent or bot is, the
/// person's own agent included: unknown and forbidden are the same answer, for reading as for
/// every change.
#[tokio::test]
async fn nobody_but_the_person_reaches_what_was_learned_about_them() {
    stage!(_dir, served, workspaces, workspace, provider, ada, bo);
    workspaces
        .keep_learned_fact(
            &ada.user_public_key,
            &workspace,
            SCOUT,
            ("Takes her coffee black.", "They said so"),
        )
        .unwrap();
    let hers = provider.ask(&ada, (LIST, json!({}))).await.unwrap();
    let fact = hers.facts.first().expect("her fact").id.clone();
    let changes = || {
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

    // Another member's list is their own: empty, and her fact is no fact of it.
    let his = provider.ask(&bo, (LIST, json!({}))).await.unwrap();
    assert!(his.facts.is_empty() && his.revision.is_empty(), "{his:?}");
    for asking in [
        (
            CORRECT,
            json!({"fact": fact, "text": "Takes her coffee white."}),
        ),
        (FORGET, json!({"fact": fact})),
    ] {
        let refused = provider.ask(&bo, asking).await;
        assert!(matches!(refused, Err(Refused { .. })), "{refused:?}");
    }

    // A session that is not the caller's own: he on her session, she named on his.
    for (device, named) in [(&ada, caller(&bo)), (&bo, caller(&ada))] {
        for asking in changes() {
            let action = asking.0;
            let refused = provider.ask_as(device, &named, asking).await;
            assert!(
                matches!(refused, Err(UnknownAction)),
                "{action}: {refused:?}"
            );
        }
    }

    // No agent and no bot, her own agent included.
    let person = hex::encode(ada.user_public_key);
    let agents = [
        Caller::delegated_agent(SCOUT, ada.user_public_key, ada.device_public_key),
        Caller::bot("installed", ada.user_public_key, ada.device_public_key),
        Caller::agent_turn(
            SCOUT,
            person,
            tilo_action_policy::turn::Turn::new("chat/main", "message-1"),
        ),
        Caller::local_agent(SCOUT),
        Caller::authenticated(ada.user_public_key, ada.device_public_key).delegated_to(SCOUT),
    ];
    for agent in &agents {
        for asking in changes() {
            let action = asking.0;
            let refused = provider.ask_as(&ada, agent, asking).await;
            assert!(
                matches!(refused, Err(UnknownAction)),
                "{action} as {:?}: {refused:?}",
                agent.acting()
            );
        }
    }

    // Her list is as it was, and he still has none.
    assert_eq!(provider.ask(&ada, (LIST, json!({}))).await.unwrap(), hers);
    assert!(
        workspaces
            .learned_about(&bo.user_public_key, &workspace)
            .is_none()
    );
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

/// What a server keeps of what was learned about a person is kept for that person in that
/// workspace: another person's and another workspace's are resources of their own, a change that
/// fails changes nothing, nothing that is no workspace id names a file, and on disk each resource
/// lies under its own person's folder, readable by the server's user alone.
#[tokio::test]
async fn what_was_learned_is_kept_for_one_person_in_one_workspace() {
    const ADA: [u8; 32] = [7; 32];
    const BO: [u8; 32] = [9; 32];
    stage!(dir, served, workspaces, here, _provider, _ada, _bo);
    let there = workspaces.create("There").await.unwrap().workspace;
    let texts = |person: &[u8; 32], workspace: &str| -> Vec<String> {
        workspaces
            .learned_about(person, workspace)
            .map(|learned| {
                learned
                    .facts
                    .iter()
                    .map(|(_, fact)| fact.text.clone())
                    .collect()
            })
            .unwrap_or_default()
    };
    assert!(workspaces.learned_about(&ADA, &here).is_none());
    workspaces
        .keep_learned_fact(
            &ADA,
            &here,
            SCOUT,
            ("Takes her coffee black.", "They said so"),
        )
        .unwrap();
    assert_eq!(texts(&ADA, &here), ["Takes her coffee black."]);
    assert!(
        workspaces.learned_about(&BO, &here).is_none(),
        "nobody else's"
    );
    assert!(
        workspaces.learned_about(&ADA, &there).is_none(),
        "no other workspace's"
    );
    workspaces
        .keep_learned_fact(&BO, &here, SCOUT, ("Drinks tea.", "They said so"))
        .unwrap();
    assert_eq!(texts(&ADA, &here), ["Takes her coffee black."]);
    assert_eq!(texts(&BO, &here), ["Drinks tea."]);

    // A change that refuses keeps the resource as it was.
    let refused: Result<(), String> =
        workspaces.change_learned(&ADA, &here, Some(SCOUT), |_| Err("refused".to_owned()));
    assert_eq!(refused, Err("refused".to_owned()));
    assert!(
        workspaces
            .keep_learned_fact(&ADA, &here, SCOUT, ("two\nlines", "They said so"))
            .is_err()
    );
    assert_eq!(texts(&ADA, &here), ["Takes her coffee black."]);

    // Only a workspace's id names a resource: nothing else becomes part of a path.
    for no_workspace in [
        "",
        "..",
        "../../people",
        "a/b",
        here.trim_end_matches(char::is_alphanumeric),
    ] {
        assert!(
            workspaces.learned_about(&ADA, no_workspace).is_none(),
            "{no_workspace:?}"
        );
        assert!(
            workspaces
                .keep_learned_fact(&ADA, no_workspace, SCOUT, ("Likes tea.", "They said so"))
                .is_err(),
            "{no_workspace:?}"
        );
    }

    // On disk: two resources, each under its own person's folder, for the server's user alone.
    let kept = kept_files(dir.path());
    assert_eq!(kept.len(), 2, "{kept:?}");
    for person in [ADA, BO] {
        let theirs: Vec<_> = kept
            .iter()
            .filter(|path| path.to_string_lossy().contains(&hex::encode(person)))
            .collect();
        let [path] = theirs.as_slice() else {
            panic!("one resource of each person's own, not {theirs:?}");
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{path:?}");
        }
    }
    served.shutdown().await;
}

/// Dated facts keep concrete start/end dates alongside the person's words, render fresh relative
/// phrasing for any given day without stale prose, schedule boundary wakes at one month before
/// start, at start, and at end, and stay backwards compatible on the wire.
#[tokio::test]
async fn dated_facts_keep_windows_render_fresh_phrasing_and_schedule_boundary_wakes() {
    stage!(dir, served, workspaces, workspace, provider, ada, _bo);
    let date = |s: &str| IsoDate::parse(s).unwrap();

    // Backwards-compatible wire decoding: a legacy FactView JSON without date or confirmation
    // fields still deserializes cleanly, and absent/false fields stay omitted when serialized.
    let legacy_json = json!({
        "id": "f1",
        "text": "Takes her coffee black.",
        "source": KEPT_IN,
        "atMs": 1_700_000_000_000_i64,
        "by": SCOUT
    });
    let legacy: FactView = serde_json::from_value(legacy_json).unwrap();
    assert_eq!(legacy.starts_on, None);
    assert_eq!(legacy.ends_on, None);
    assert!(!legacy.confirmed);
    assert_eq!(legacy.effective_authority(), FactAuthority::LargeModel);
    let reserialized = serde_json::to_value(&legacy).unwrap();
    assert!(reserialized.get("startsOn").is_none());
    assert!(reserialized.get("endsOn").is_none());
    assert!(reserialized.get("confirmed").is_none());

    // Person stores a time-relative fact with concrete dates.
    let raise = json!({
        "text": "We plan to raise a round.",
        "source": "I wrote it down on 17 Nov 2026",
        "startsOn": "2027-02-17",
        "endsOn": "2027-05-17"
    });
    let kept = provider.ask(&ada, (LEARN, raise)).await.unwrap();
    let [fact] = kept.facts.as_slice() else {
        panic!("one dated fact, not {kept:?}");
    };
    assert_eq!(fact.starts_on.as_deref(), Some("2027-02-17"));
    assert_eq!(fact.ends_on.as_deref(), Some("2027-05-17"));
    assert!(fact.confirmed);
    assert_eq!(fact.authority, Some(FactAuthority::Person));

    // Fresh relative phrasing updates deterministically as the calendar advances.
    assert_eq!(
        fact.render_relative(date("2026-11-17")).as_deref(),
        Some("in 3 to 6 months")
    );
    assert_eq!(
        fact.render_relative(date("2026-12-17")).as_deref(),
        Some("in 2 to 5 months")
    );
    assert_eq!(
        fact.render_for_date(date("2026-12-17")),
        "We plan to raise a round. [in 2 to 5 months; 2027-02-17..2027-05-17]"
    );
    assert_eq!(
        fact.render_relative(date("2027-02-17")).as_deref(),
        Some("active (ends in 3 months)")
    );
    assert_eq!(
        fact.render_relative(date("2027-06-17")).as_deref(),
        Some("passed (1 month ago)")
    );

    // Deterministic scheduler wakes at 1 month before starts_on, at starts_on, and at ends_on.
    let wakes =
        workspaces.upcoming_fact_wakes(&ada.user_public_key, &workspace, date("2026-12-17"));
    assert_eq!(
        wakes,
        vec![
            FactBoundaryWake {
                fact_id: fact.id.clone(),
                text: "We plan to raise a round.".to_owned(),
                on: date("2027-01-17"),
                boundary: WindowBoundary::OneMonthBeforeStart,
                confirmed: true,
            },
            FactBoundaryWake {
                fact_id: fact.id.clone(),
                text: "We plan to raise a round.".to_owned(),
                on: date("2027-02-17"),
                boundary: WindowBoundary::WindowStart,
                confirmed: true,
            },
            FactBoundaryWake {
                fact_id: fact.id.clone(),
                text: "We plan to raise a round.".to_owned(),
                on: date("2027-05-17"),
                boundary: WindowBoundary::WindowEnd,
                confirmed: true,
            },
        ]
    );
    let after_start =
        workspaces.upcoming_fact_wakes(&ada.user_public_key, &workspace, date("2027-02-18"));
    assert_eq!(after_start.len(), 1);
    assert_eq!(after_start[0].boundary, WindowBoundary::WindowEnd);
    assert!(
        workspaces
            .upcoming_fact_wakes(&ada.user_public_key, &workspace, date("2027-05-18"))
            .is_empty()
    );

    // Correcting text without naming dates preserves the existing date window.
    let updated = provider
        .ask(
            &ada,
            (
                CORRECT,
                json!({
                    "fact": fact.id,
                    "text": "We plan to raise our seed round."
                }),
            ),
        )
        .await
        .unwrap();
    assert_eq!(updated.facts[0].starts_on.as_deref(), Some("2027-02-17"));
    assert_eq!(updated.facts[0].ends_on.as_deref(), Some("2027-05-17"));

    // Updating with snake_case date keys works, and empty string clears a date.
    let cleared_end = provider
        .ask(
            &ada,
            (
                CORRECT,
                json!({
                    "fact": fact.id,
                    "text": "We plan to raise our seed round.",
                    "starts_on": "2027-03-01",
                    "ends_on": ""
                }),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        cleared_end.facts[0].starts_on.as_deref(),
        Some("2027-03-01")
    );
    assert_eq!(cleared_end.facts[0].ends_on, None);

    // Invalid dates and inverted ranges are refused without altering kept facts.
    for bad in [
        (
            LEARN,
            json!({
                "text": "Bad leap day.",
                "source": "I wrote it down",
                "startsOn": "2027-02-29"
            }),
        ),
        (
            LEARN,
            json!({
                "text": "Inverted window.",
                "source": "I wrote it down",
                "startsOn": "2027-06-01",
                "endsOn": "2027-05-01"
            }),
        ),
        (
            CORRECT,
            json!({
                "fact": fact.id,
                "text": "Still bad.",
                "endsOn": "2027-02-01"
            }),
        ),
    ] {
        let refused = provider.ask(&ada, bad).await;
        assert!(matches!(refused, Err(Refused { .. })), "{refused:?}");
    }
    assert_eq!(provider.list(&ada).await, cleared_end);

    // Sidecar metadata file is kept owner-only (0600) beside the automerge document.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let details_path = kept_files(dir.path())
            .into_iter()
            .find(|path| {
                path.to_string_lossy()
                    .contains(&hex::encode(ada.user_public_key))
            })
            .expect("automerge document for ada")
            .with_file_name(format!("{workspace}-details.json"));
        let mode = std::fs::metadata(&details_path)
            .expect("sidecar details file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Forgetting the fact removes its wakes and cleans up the sidecar metadata.
    let empty = provider
        .ask(&ada, (FORGET, json!({"fact": fact.id})))
        .await
        .unwrap();
    assert!(empty.facts.is_empty());
    assert!(
        workspaces
            .upcoming_fact_wakes(&ada.user_public_key, &workspace, date("2026-01-01"))
            .is_empty()
    );
    served.shutdown().await;
}

/// Authority order (`Person > LargeModel > DecisionModel`) is enforced on every update:
/// a fact set or confirmed by the person cannot be silently overwritten or forgotten by a model
/// inference without explicit user confirmation.
#[tokio::test]
async fn confirmed_facts_cannot_be_silently_overwritten_by_model_inference() {
    stage!(_dir, served, workspaces, workspace, provider, ada, _bo);

    // 1. Decision model writes an unconfirmed fact.
    let fact_id = workspaces
        .keep_dated_fact(
            &ada.user_public_key,
            &workspace,
            Some(SCOUT),
            FactUpdate {
                text: "Raising sometime in spring.".to_owned(),
                source: "Decision inference".to_owned(),
                starts_on: Some("2027-03-01".to_owned()),
                ends_on: Some("2027-05-31".to_owned()),
                authority: FactAuthority::DecisionModel,
            },
        )
        .unwrap();
    let listed = provider.list(&ada).await;
    assert_eq!(listed.facts.len(), 1);
    assert!(!listed.facts[0].confirmed);
    assert_eq!(
        listed.facts[0].authority,
        Some(FactAuthority::DecisionModel)
    );

    // 2. Large model outranks DecisionModel and may refine the unconfirmed fact.
    workspaces
        .correct_dated_fact(
            &ada.user_public_key,
            &workspace,
            Some(SCOUT),
            &fact_id,
            FactCorrection {
                text: "Raising between February and May 2027.".to_owned(),
                source: Some("Large model synthesis".to_owned()),
                starts_on: Some(Some("2027-02-17".to_owned())),
                ends_on: Some(Some("2027-05-17".to_owned())),
                authority: FactAuthority::LargeModel,
            },
        )
        .unwrap();
    let after_large = provider.list(&ada).await;
    assert!(!after_large.facts[0].confirmed);
    assert_eq!(
        after_large.facts[0].authority,
        Some(FactAuthority::LargeModel)
    );

    // 3. DecisionModel cannot overwrite a LargeModel fact.
    let decision_overwrite = workspaces.correct_dated_fact(
        &ada.user_public_key,
        &workspace,
        Some(SCOUT),
        &fact_id,
        FactCorrection {
            text: "Not raising.".to_owned(),
            source: Some("Noisy classifier".to_owned()),
            starts_on: None,
            ends_on: None,
            authority: FactAuthority::DecisionModel,
        },
    );
    assert!(
        decision_overwrite.is_err(),
        "DecisionModel must not overwrite LargeModel"
    );

    // 4. The person confirms the agent's fact through the provider (`CONFIRM`).
    let confirmed = provider
        .ask(
            &ada,
            (
                CONFIRM,
                json!({
                    "fact": fact_id,
                    "startsOn": "2027-02-17",
                    "endsOn": "2027-05-17"
                }),
            ),
        )
        .await
        .unwrap();
    assert!(confirmed.facts[0].confirmed);
    assert_eq!(confirmed.facts[0].authority, Some(FactAuthority::Person));
    assert_ne!(confirmed.revision, after_large.revision);

    // 5. Neither LargeModel nor DecisionModel may overwrite, re-learn over, or forget a confirmed
    // fact without explicit user confirmation.
    assert!(
        workspaces
            .correct_dated_fact(
                &ada.user_public_key,
                &workspace,
                Some(SCOUT),
                &fact_id,
                FactCorrection {
                    text: "Already raising now.".to_owned(),
                    source: Some("Inferred from inbound mail".to_owned()),
                    starts_on: None,
                    ends_on: None,
                    authority: FactAuthority::LargeModel,
                },
            )
            .is_err()
    );
    assert!(
        workspaces
            .keep_learned_fact(
                &ada.user_public_key,
                &workspace,
                SCOUT,
                (
                    "raising between february and may 2027.",
                    "Silent overwrite attempt"
                ),
            )
            .is_err()
    );
    let forget_attempt =
        workspaces.change_learned(&ada.user_public_key, &workspace, Some(SCOUT), |resource| {
            resource
                .apply(tilo_workspace_agent::learned::Learned::FORGET.with(
                    tilo_workspace_agent::learned::learned::Forget {
                        fact: tilo_resource::Id::from_raw(fact_id.clone()),
                    },
                ))
                .map(drop)
                .map_err(|error| error.to_string())
        });
    assert!(
        forget_attempt.is_err(),
        "an agent must not silently forget a confirmed fact"
    );

    // Confirm nothing changed on disk after all refused model writes.
    assert_eq!(provider.list(&ada).await, confirmed);

    // 6. An agent may still add a brand-new unrelated fact without touching the confirmed fact.
    workspaces
        .keep_learned_fact(
            &ada.user_public_key,
            &workspace,
            SCOUT,
            ("Prefers concise weekly briefs.", KEPT_IN),
        )
        .unwrap();
    let with_second = provider.list(&ada).await;
    assert_eq!(with_second.facts.len(), 2);
    let still_confirmed = with_second
        .facts
        .iter()
        .find(|item| item.id == fact_id)
        .unwrap();
    assert!(still_confirmed.confirmed);
    assert_eq!(
        still_confirmed.text,
        "Raising between February and May 2027."
    );

    // 7. An update carrying explicit user confirmation (`FactAuthority::Person`) IS allowed.
    workspaces
        .correct_dated_fact(
            &ada.user_public_key,
            &workspace,
            Some(SCOUT),
            &fact_id,
            FactCorrection {
                text: "Raising between March and June 2027.".to_owned(),
                source: Some("Confirmed by person at 1-month boundary".to_owned()),
                starts_on: Some(Some("2027-03-17".to_owned())),
                ends_on: Some(Some("2027-06-17".to_owned())),
                authority: FactAuthority::Person,
            },
        )
        .unwrap();
    let updated = provider.list(&ada).await;
    let moved = updated
        .facts
        .iter()
        .find(|item| item.id == fact_id)
        .unwrap();
    assert_eq!(moved.text, "Raising between March and June 2027.");
    assert_eq!(moved.starts_on.as_deref(), Some("2027-03-17"));
    assert_eq!(moved.ends_on.as_deref(), Some("2027-06-17"));
    assert!(moved.confirmed);

    served.shutdown().await;
}
