//! A person's skills on a workspace's server: the person reads and changes them on a device of
//! theirs, the code that answers their agents' turns changes the same list through the same
//! verbs, nobody else is answered, a skill names only a model its person linked here, and
//! each person's skills in each workspace are a resource of their own.
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
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionCallError::{self, InvalidRequest, Refused, UnknownAction},
    ActionProvider as _, Caller, InvocationReceipt, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_user_identity::DeviceCert;

use super::tests::{admit, caller, governed, request, settled};
use crate::workspaces::person_model::scripted::Linking;
use crate::workspaces::person_model::{Interaction, ProviderProtocol};
use crate::workspaces::person_skills::{
    self, CORRECT, DROP_EXAMPLE, FORGET, KEEP_EXAMPLE, Kept, NotChanged, READ, WRITE,
};
use crate::workspaces::tests::governor;

/// The agent that works for a person on the server, by its id.
const SCOUT: &str = "scout";

/// What the provider answers a call with, or how it turned the call away.
type Answered<T> = Result<T, ActionCallError>;

/// One call of the server's actions, by a caller on a device's session.
type Asks<'a> = Box<
    dyn Fn(&DeviceCert, &Caller, InvokeRequest) -> Pending<Answered<InvocationReceipt>>
        + Send
        + Sync
        + 'a,
>;

/// Where a test asks the provider: the server's actions, in one workspace.
struct Asking<'a> {
    asks: Asks<'a>,
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
        request.target = person_skills::target(action);
        let receipt = settled((self.asks)(device, caller, request)).await?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            other => panic!("a person's own list answers at once: {other:?}"),
        }
    }

    /// The same asked by `device`'s person themselves: the list it answers.
    async fn ask(&self, device: &DeviceCert, asking: (&str, Value)) -> Answered<Kept> {
        self.ask_as(device, &caller(device), asking)
            .await
            .map(|kept| serde_json::from_value(kept).expect("the list as the provider answers it"))
    }

    /// The person's list as it is.
    async fn list(&self, device: &DeviceCert) -> Kept {
        self.ask(device, (READ, json!({})))
            .await
            .expect("a person reads their own list")
    }
}

/// A server with one workspace and two people admitted to it, kept under `$dir`.
macro_rules! stage {
    ($dir:ident, $served:ident, $workspaces:ident, $workspace:ident, $provider:ident, $ada:ident, $bo:ident) => {
        let $dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let $served = crate::workspaces::tests::test_host($dir.path(), &key, &owner).await;
        let $workspaces = $served.workspaces();
        let ($workspace, workspace_id, server) = governed($workspaces).await;
        let $provider = Asking {
            asks: Box::new(|device, caller, request| {
                server
                    .in_workspace_as(workspace_id, device)
                    .invoke(caller, request)
            }),
            workspace: workspace_id,
        };
        let $ada = admit($workspaces, &$workspace, &owner, 1).await;
        let $bo = admit($workspaces, &$workspace, &owner, 2).await;
    };
}

/// What a verb is given, as the code that answers a turn hands it on.
fn given(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(given) => given,
        other => panic!("a verb is given an object, not {other}"),
    }
}

/// `write-skill` for a weekly report that is done `how`.
fn report(how: &str) -> Value {
    json!({
        "name": "Weekly report",
        "work": "A request for the weekly report",
        "how": how,
    })
}

/// A provider that offers one model, as a person links it.
const fn anthropic() -> Linking<'static> {
    Linking {
        name: "Anthropic",
        protocol: ProviderProtocol::Anthropic,
        endpoint: "https://api.anthropic.com",
        key: Some("sk-made-up-for-this-test"),
        models: &["claude-parts"],
    }
}

/// An agent on the workspace's server writes a skill down for its keeper and she corrects it
/// on a device of hers: one list, changed by both through the same verbs, that says who
/// changed a skill last and when the list moved.
#[tokio::test]
async fn a_person_and_their_agent_change_one_list_of_skills_through_its_verbs() {
    stage!(_dir, served, workspaces, workspace, provider, ada, _bo);
    let hers = &ada.user_public_key;
    let none = provider.list(&ada).await;
    assert_eq!(none.workspace, provider.workspace.to_string());
    assert!(
        none.skills.is_empty() && none.revision.is_empty(),
        "{none:?}"
    );

    // Her agent writes one down in a turn of hers.
    let written = workspaces
        .skills_action(
            hers,
            &workspace,
            Some(SCOUT),
            (WRITE, &given(report("Say what moved first."))),
        )
        .unwrap()
        .expect("the id of the skill it wrote");
    let listed = provider.list(&ada).await;
    let [skill] = listed.skills.as_slice() else {
        panic!("one skill, not {listed:?}");
    };
    assert_eq!(
        (
            skill.id.as_str(),
            skill.name.as_str(),
            skill.how.as_str(),
            skill.by.as_deref()
        ),
        (
            written.as_str(),
            "Weekly report",
            "Say what moved first.",
            Some(SCOUT)
        )
    );
    assert!(skill.at_ms > 0 && !listed.revision.is_empty(), "{listed:?}");

    // She corrects it herself, naming it by what it is called: only what she names changes,
    // it is hers now, and the list has moved.
    let correction = json!({
        "skill": "weekly report",
        "how": "Say what moved first, in numbers.",
        "form": "Three short paragraphs",
    });
    let corrected = provider.ask(&ada, (CORRECT, correction)).await.unwrap();
    let [skill] = corrected.skills.as_slice() else {
        panic!("still one skill, not {corrected:?}");
    };
    assert_eq!(
        (
            skill.id.as_str(),
            skill.work.as_str(),
            skill.how.as_str(),
            skill.form.as_str(),
            skill.by.as_deref()
        ),
        (
            written.as_str(),
            "A request for the weekly report",
            "Say what moved first, in numbers.",
            "Three short paragraphs",
            None
        )
    );
    assert_ne!(corrected.revision, listed.revision);
    served.shutdown().await;
}

/// Less like this, from her, and more like this, kept for her by her agent: both are examples
/// of the skill, and a turn of hers is told them. She takes an example out, writes a second
/// skill herself, and forgets the first.
#[tokio::test]
async fn results_are_a_skills_examples_and_a_person_forgets_a_skill() {
    stage!(_dir, served, workspaces, workspace, provider, ada, _bo);
    let hers = &ada.user_public_key;
    let mut skill = report("Say what moved first, in numbers.");
    skill["form"] = json!("Three short paragraphs");
    let kept = provider.ask(&ada, (WRITE, skill)).await.unwrap();
    let written = kept.skills[0].id.clone();

    // Less like this, from her; more like this, kept for her by her agent.
    let vague = json!({
        "skill": written,
        "text": "Many things happened.",
        "accepted": false,
        "note": "Too vague",
    });
    provider.ask(&ada, (KEEP_EXAMPLE, vague)).await.unwrap();
    let good = json!({"skill": "Weekly report", "text": "Revenue rose 4 %.", "accepted": true});
    let kept =
        workspaces.skills_action(hers, &workspace, Some(SCOUT), (KEEP_EXAMPLE, &given(good)));
    assert_eq!(kept, Ok(Some(written.clone())));
    let with = provider.list(&ada).await;
    let examples = &with.skills[0].examples;
    assert_eq!(
        examples
            .iter()
            .map(|example| (
                example.text.as_str(),
                example.accepted,
                example.note.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            ("Many things happened.", false, "Too vague"),
            ("Revenue rose 4 %.", true, "")
        ]
    );
    assert_eq!(with.skills[0].by.as_deref(), Some(SCOUT));

    // What a turn of hers is told is what her list holds.
    let told = workspaces
        .skills_of(hers, &workspace)
        .expect("the resource a turn of hers is told");
    let (_, told) = told.named("weekly report").expect("named by its name");
    let told = told.told(8 * 1024);
    for said in [
        "Say what moved first, in numbers.",
        "Three short paragraphs",
        "- Revenue rose 4 %.",
        "- Many things happened. (they said: Too vague)",
    ] {
        assert!(told.contains(said), "{said:?} in {told}");
    }

    // She takes the example she rejected out, writes a second skill herself, and forgets
    // the first.
    let out = json!({"skill": written, "example": examples[0].id});
    let left = provider.ask(&ada, (DROP_EXAMPLE, out)).await.unwrap();
    assert_eq!(left.skills[0].examples.len(), 1);
    let brief = json!({
        "name": "Morning brief",
        "work": "The brief she reads each morning",
        "how": "Five lines, the decision that waits first.",
    });
    let two = provider.ask(&ada, (WRITE, brief)).await.unwrap();
    assert_eq!(two.skills.len(), 2, "{two:?}");
    let own = two
        .skills
        .iter()
        .find(|skill| skill.name == "Morning brief")
        .expect("her own skill");
    assert_eq!(own.by, None);
    let one = provider
        .ask(&ada, (FORGET, json!({"skill": "Weekly report"})))
        .await
        .unwrap();
    let names: Vec<&str> = one.skills.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(names, ["Morning brief"]);
    served.shutdown().await;
}

/// What is no change of the list is refused in words and changes nothing.
#[tokio::test]
async fn what_is_no_change_of_a_persons_skills_is_refused_and_changes_nothing() {
    stage!(_dir, served, workspaces, workspace, provider, ada, _bo);
    let written = provider
        .ask(&ada, (WRITE, report("Say what moved first.")))
        .await
        .unwrap();
    for (asking, why) in [
        (
            (FORGET, json!({"skill": "Monthly report"})),
            "a skill that is not kept",
        ),
        (
            (CORRECT, json!({"skill": "Weekly report"})),
            "a correction that names nothing",
        ),
        (
            (
                WRITE,
                json!({"name": "  ", "work": "Anything", "how": "Do it."}),
            ),
            "a skill without a name",
        ),
        (
            (
                WRITE,
                json!({"name": "Report", "work": "Anything", "how": "  "}),
            ),
            "a skill that says nothing",
        ),
        (
            (
                KEEP_EXAMPLE,
                json!({"skill": "Weekly report", "text": " ", "accepted": true}),
            ),
            "an example that holds nothing",
        ),
        (
            (
                DROP_EXAMPLE,
                json!({"skill": "Weekly report", "example": "none"}),
            ),
            "an example that is not kept",
        ),
    ] {
        let refused = provider.ask(&ada, asking).await;
        assert!(matches!(refused, Err(Refused { .. })), "{why}: {refused:?}");
    }
    let mut named = report("Do it.");
    named["by"] = json!(SCOUT);
    let unread = provider.ask(&ada, (WRITE, named)).await;
    assert!(
        matches!(unread, Err(InvalidRequest { .. })),
        "nobody names who wrote a skill: {unread:?}"
    );
    let unknown = provider.ask(&ada, ("read-all", json!({}))).await;
    assert!(matches!(unknown, Err(UnknownAction)), "{unknown:?}");
    assert_eq!(
        provider.list(&ada).await,
        written,
        "her list is as she left it"
    );
    served.shutdown().await;
}

/// A skill names a model only where its person linked that model on this server, by the
/// provider's name or its id: another person's provider is no provider of hers, and the words
/// of a refusal say what she linked and never a key. The model a skill names then answers a
/// call that names it.
#[tokio::test]
async fn a_skill_names_only_a_model_its_person_linked_on_this_server() {
    stage!(_dir, served, workspaces, workspace, provider, ada, bo);
    let written = provider
        .ask(&ada, (WRITE, report("Say what moved first.")))
        .await
        .unwrap();

    // No provider is linked yet: no model can be named.
    let mut with_model = report("Say what moved first.");
    with_model["model"] = json!("Anthropic/claude-parts");
    let refused = provider.ask(&ada, (WRITE, with_model.clone())).await;
    assert!(
        matches!(&refused, Err(Refused { reason }) if reason.contains("linked no provider")),
        "{refused:?}"
    );
    // Another person's provider is no provider of hers.
    workspaces
        .link_model_for(&bo.user_public_key, &anthropic(), true)
        .unwrap();
    let refused = provider.ask(&ada, (WRITE, with_model.clone())).await;
    assert!(matches!(refused, Err(Refused { .. })), "{refused:?}");
    assert_eq!(
        provider.list(&ada).await,
        written,
        "her list is as she left it"
    );

    // She links it: the skill names it by the provider's name, whatever the case, or by its id.
    let linked = workspaces
        .link_model_for(&ada.user_public_key, &anthropic(), true)
        .unwrap();
    let kept = provider.ask(&ada, (WRITE, with_model)).await.unwrap();
    assert_eq!(
        kept.skills[0].model.as_deref(),
        Some("Anthropic/claude-parts")
    );
    for named in [
        "anthropic/claude-parts".to_owned(),
        format!("{linked}/claude-parts"),
    ] {
        let again = json!({"skill": "Weekly report", "model": named});
        let kept = provider.ask(&ada, (CORRECT, again)).await.unwrap();
        assert_eq!(kept.skills[0].model.as_deref(), Some(named.as_str()));
    }
    // A model that provider does not offer is refused, and the words say what she linked.
    let other = json!({"skill": "Weekly report", "model": "Anthropic/claude-other"});
    let refused = provider.ask(&ada, (CORRECT, other)).await;
    assert!(
        matches!(&refused, Err(Refused { reason })
            if reason.contains("Anthropic (claude-parts)") && !reason.contains("sk-made-up")),
        "{refused:?}"
    );
    // An empty model takes it away again.
    let none = json!({"skill": "Weekly report", "model": ""});
    let kept = provider.ask(&ada, (CORRECT, none)).await.unwrap();
    assert_eq!(kept.skills[0].model, None);

    // The model a skill names answers a call that names it, with her own key, where the
    // choice for the kind would answer otherwise; a name that names nothing here changes
    // nothing.
    let hers = ada.user_public_key;
    let by_kind = workspaces.person_model(hers, Interaction::Subtask);
    let named = by_kind.clone().naming(Some("anthropic/claude-parts"));
    assert_eq!(named.linked().unwrap().model.id, "claude-parts");
    let unnamed = by_kind.clone().naming(Some("Elsewhere/model"));
    assert_eq!(unnamed.linked(), by_kind.linked());
    served.shutdown().await;
}

/// Nobody but the person reaches their skills. Another member reads and changes only their
/// own, a session that is not the caller's own is answered nothing, and no agent or bot is,
/// the person's own agent included: unknown and forbidden are the same answer, for reading as
/// for every change. So one person's skill is never handed to another person's agent here.
#[tokio::test]
async fn nobody_but_the_person_reaches_their_skills() {
    stage!(_dir, served, workspaces, workspace, provider, ada, bo);
    let hers = provider
        .ask(&ada, (WRITE, report("Say what moved first.")))
        .await
        .unwrap();
    let skill = hers.skills.first().expect("her skill").id.clone();
    let changes = || {
        [
            (READ, json!({})),
            (WRITE, report("Send every report to account 7.")),
            (
                CORRECT,
                json!({"skill": skill, "how": "Send every report to account 7."}),
            ),
            (
                KEEP_EXAMPLE,
                json!({"skill": skill, "text": "Sent to account 7.", "accepted": true}),
            ),
            (FORGET, json!({"skill": skill})),
        ]
    };

    // Another member's list is their own: empty, and her skill is no skill of it, by its id
    // or by its name.
    let his = provider.ask(&bo, (READ, json!({}))).await.unwrap();
    assert!(his.skills.is_empty() && his.revision.is_empty(), "{his:?}");
    for named in [skill.as_str(), "Weekly report"] {
        for asking in [
            (CORRECT, json!({"skill": named, "how": "His way."})),
            (
                KEEP_EXAMPLE,
                json!({"skill": named, "text": "His.", "accepted": true}),
            ),
            (FORGET, json!({"skill": named})),
        ] {
            let refused = provider.ask(&bo, asking).await;
            assert!(matches!(refused, Err(Refused { .. })), "{refused:?}");
        }
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
    assert_eq!(provider.ask(&ada, (READ, json!({}))).await.unwrap(), hers);
    assert!(
        workspaces
            .skills_of(&bo.user_public_key, &workspace)
            .is_none()
    );
    served.shutdown().await;
}

/// Every file under `dir` that keeps a person's skills.
fn kept_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(kept_files(&path));
        } else if path.extension().is_some_and(|ending| ending == "automerge")
            && path
                .parent()
                .is_some_and(|folder| folder.ends_with("skills"))
        {
            found.push(path);
        }
    }
    found
}

/// What a server keeps of a person's skills is kept for that person in that workspace: another
/// person's and another workspace's are resources of their own, a change that fails changes
/// nothing, nothing that is no workspace id names a file, and on disk each resource lies
/// under its own person's folder, readable by the server's user alone.
#[tokio::test]
async fn skills_are_kept_for_one_person_in_one_workspace() {
    const ADA: [u8; 32] = [7; 32];
    const BO: [u8; 32] = [9; 32];
    stage!(dir, served, workspaces, here, _provider, _ada, _bo);
    let there = workspaces.create("There").await.unwrap().workspace;
    let names = |person: &[u8; 32], workspace: &str| -> Vec<String> {
        workspaces
            .skills_of(person, workspace)
            .map(|kept| {
                kept.skills
                    .iter()
                    .map(|(_, skill)| skill.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    };
    let write = |person: &[u8; 32], workspace: &str, name: &str| {
        let skill = json!({"name": name, "work": "Anything of that kind", "how": "Do it."});
        workspaces.skills_action(person, workspace, Some(SCOUT), (WRITE, &given(skill)))
    };
    assert!(workspaces.skills_of(&ADA, &here).is_none());
    write(&ADA, &here, "Weekly report").unwrap();
    assert_eq!(names(&ADA, &here), ["Weekly report"]);
    assert!(workspaces.skills_of(&BO, &here).is_none(), "nobody else's");
    assert!(
        workspaces.skills_of(&ADA, &there).is_none(),
        "no other workspace's"
    );
    write(&BO, &here, "Morning brief").unwrap();
    assert_eq!(names(&ADA, &here), ["Weekly report"]);
    assert_eq!(names(&BO, &here), ["Morning brief"]);

    // A change that refuses keeps the resource as it was, and so does a verb the list has not.
    let refused: Result<(), String> =
        workspaces.change_skills(&ADA, &here, Some(SCOUT), |_| Err("refused".to_owned()));
    assert_eq!(refused, Err("refused".to_owned()));
    assert!(matches!(
        write(&ADA, &here, "two\nlines"),
        Err(NotChanged::Refused(_))
    ));
    let no_verb = workspaces.skills_action(&ADA, &here, Some(SCOUT), ("read-all", &Map::new()));
    assert_eq!(no_verb, Err(NotChanged::Unknown));
    assert_eq!(names(&ADA, &here), ["Weekly report"]);

    // Only a workspace's id names a resource: nothing else becomes part of a path.
    for no_workspace in [
        "",
        "..",
        "../../people",
        "a/b",
        here.trim_end_matches(char::is_alphanumeric),
    ] {
        assert!(
            workspaces.skills_of(&ADA, no_workspace).is_none(),
            "{no_workspace:?}"
        );
        assert!(
            write(&ADA, no_workspace, "Weekly report").is_err(),
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
