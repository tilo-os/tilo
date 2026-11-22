//! Skills use the workspace's ordinary catalogue and document actions.

use super::*;
use chat::skills::{Skills, skills::WriteSkill};
use serde_json::json;
use tilo_actions::remote::{ActionProvider as _, CatalogueQuery};
use tilo_peer_state::join::Role;
use tilo_server::workspace_agents::WorkspaceActionProvider;

const SKILLS: &str = chat::skills::KIND;
const INSTRUCTIONS: &str = "Put the weekly numbers first.";
const CORRECTION: &str = "Put revenue and its source first.";

fn draft() -> serde_json::Value {
    json!({
        "name": "Weekly report",
        "work": "Write the weekly report",
        "how": INSTRUCTIONS,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_skills_are_shared_and_use_the_members_role() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "server", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (served, actions) = crate::harness::hosted::serving_documents_with_actions(
        &root,
        &device,
        state.path(),
        crate::common::catalogue,
    )
    .await;
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let person = hex::encode(root.verifying_key().to_bytes());
    let legacy = state
        .path()
        .join("server/workspaces/people")
        .join(&person)
        .join("skills");
    std::fs::create_dir_all(&legacy).unwrap();
    let legacy = legacy.join(format!("{workspace}.automerge"));
    let mut private = Replica::<Skills>::new().as_author(&person);
    private
        .apply(Skills::WRITE_SKILL.with(WriteSkill {
            name: NonBlank::parse("Private report").unwrap(),
            work: NonBlank::parse("Earlier private work").unwrap(),
            how: "Private instructions".into(),
            form: None,
            model: None,
        }))
        .unwrap();
    let private = private.save();
    std::fs::write(&legacy, &private).unwrap();
    let page = Browser::open_at(&served, &browser, &device, &workspace).await;
    let catalogue = crate::common::catalogue();
    workspaces
        .prepare_application(&workspace, &device.cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let owner = themselves(&root, &browser, &workspace);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let guest_device = enrol(&guest, "bo-browser", DeviceCapabilities::member());
    crate::workspace_actions::resource_support::admit_person_at(
        &served,
        &device,
        &workspace,
        &guest_device,
        Role::Viewer,
    )
    .await;
    let guest_page = crate::workspace_actions::resource_support::open_admitted_at(
        &served,
        &guest_device,
        &device,
        &workspace,
    )
    .await;
    let viewer = themselves(&guest, &guest_device, &workspace);

    let provider =
        WorkspaceActionProvider::new(actions.clone(), WorkspaceId::parse(&workspace).unwrap());
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    let published = provider
        .query(
            &owner,
            CatalogueQuery {
                provider: Some(tilo_agent_session::resource_provider(&route)),
                resource_name: Some(SKILLS.into()),
                ..CatalogueQuery::default()
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .map_err(|_| "the workspace catalogue did not answer")
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        published
            .rows
            .iter()
            .any(|row| row.action.as_ref().is_some_and(|action| {
                action.address.resource == SKILLS && action.address.action == "write-skill"
            })),
        "{published:?}"
    );
    let blank = chat_action(
        &actions,
        &workspace,
        &owner,
        ("", SKILLS),
        "read",
        json!({}),
    )
    .await
    .expect("the workspace's empty Skills resource reads");
    assert_eq!(blank["value"]["skills"], json!([]), "{blank}");

    chat_action(
        &actions,
        &workspace,
        &owner,
        ("", SKILLS),
        "write-skill",
        draft(),
    )
    .await
    .expect("the owner can add a workspace skill");
    let read = chat_action(
        &actions,
        &workspace,
        &viewer,
        ("", SKILLS),
        "read",
        json!({}),
    )
    .await
    .expect("a second member can read the shared skill");
    assert!(read.to_string().contains(INSTRUCTIONS), "{read}");
    let denied = chat_action(
        &actions,
        &workspace,
        &viewer,
        ("", SKILLS),
        "write-skill",
        json!({"name": "Viewer entry", "work": "Review", "how": "Write a summary."}),
    )
    .await
    .expect_err("a viewer cannot write workspace skills");
    assert_eq!(denied, "the caller may not invoke this action");
    let again = chat_action(
        &actions,
        &workspace,
        &owner,
        ("", SKILLS),
        "read",
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        again["value"], read["value"],
        "a denied write leaves the record alone"
    );

    let other = workspaces.create("Separate team").await.unwrap().workspace;
    let other_page = Browser::open_at(&served, &browser, &device, &other).await;
    workspaces
        .prepare_application(&other, &device.cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let other_owner = themselves(&root, &browser, &other);
    let empty = chat_action(
        &actions,
        &other,
        &other_owner,
        ("", SKILLS),
        "read",
        json!({}),
    )
    .await
    .expect("the second workspace has its own Skills resource");
    assert_eq!(empty["value"]["skills"], json!([]), "{empty}");
    assert_eq!(std::fs::read(legacy).unwrap(), private);
    other_page.peer.close();
    guest_page.peer.close();
    page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosted_agent_uses_the_same_workspace_skills_actions() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let owner = themselves(&root, &browser, &workspace);
    chat_action(
        &actions,
        &workspace,
        &owner,
        ("", SKILLS),
        "write-skill",
        draft(),
    )
    .await
    .unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model = plays(&asked, |request| {
        if let Some((answer, error)) = tool_answers(request).last() {
            assert!(!error, "{answer}");
            return says("I updated the workspace skill.");
        }
        does(
            0,
            "chat/Workspace Skills/Weekly report#correct-skill",
            &json!({"how": CORRECTION}),
        )
    });
    let agents = worked(&served, &workspace, model).await;
    let message = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "In Workspace Skills, correct how the weekly report is done.",
    )
    .await;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        agents.look(&workspace).await.unwrap();
        let part = conversation_part(&workspaces, &workspace).await;
        if let Some(reply) = part
            .messages
            .get(&reply_id(&message, &MemberId::agent(SCOUT)))
            && reply.text == "I updated the workspace skill."
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the agent did not update the skill"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let read = chat_action(
        &actions,
        &workspace,
        &owner,
        ("", SKILLS),
        "read",
        json!({}),
    )
    .await
    .unwrap();
    assert!(read.to_string().contains(CORRECTION), "{read}");
    assert!(read.to_string().contains(SCOUT), "{read}");
    page.peer.close();
}
