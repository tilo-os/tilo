//! The real person's hosted Assistant page reviews proposals from their agent's turn.

use super::*;
use crate::workspace_actions::hosted_page_stream::{SignedPage, button};
use tilo_server_protocol::page::{PageAsk, PageRefusal};

const REVIEW: &str = "assistant.section:proposals";

/// The independently signed device session reads what the server durably decided.
async fn persisted(browser: &mut Browser, description: &str, find: impl Fn(&Proposals) -> bool) {
    let target =
        workspace_initiative::target(workspace_initiative::PROPOSALS, workspace_initiative::LIST);
    tilo_test_env::within(description, async {
        loop {
            let receipt = browser
                .invoke_settled(request(
                    &target.provider,
                    target.address.clone(),
                    InvokeArgument::None,
                ))
                .await;
            let ReceiptStatus::Completed { output } = receipt.status else {
                panic!(
                    "the signed person can read their initiative catalogue: {:?}",
                    receipt.status
                );
            };
            let listing: Proposals =
                serde_json::from_value(output.value.expect("list returns the decisions")).unwrap();
            if find(&listing) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// Wait for the relevant rendered row, then use the numbered action in its actual button.
async fn press(page: &mut SignedPage, words: &str, find: impl Fn(&str) -> Option<PageAsk>) {
    let press = page
        .until(words, find)
        .await
        .expect("the person's review page is authorized");
    page.ask(&press).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_reviews_and_changes_proposal_consent_on_the_assistant_page() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "workspace host", DeviceCapabilities::member());
    let device = enrol(&root, "Ada's browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let workspaces = server.workspaces().unwrap();
    let workspace = workspaces
        .create("Proposal review")
        .await
        .unwrap()
        .workspace;
    let mut browser = Browser::open_at(&server, &device, &host, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    furnished(&browser, workspaces, &workspace, &person).await;
    write_to_agent(
        &server.roster.actions,
        workspaces,
        &workspace,
        (&root, &device),
        "Suggest a task or reminder.",
    )
    .await;
    let agents = server.approving(&workspace, &[SCOUT]).await;
    let turns = agents.turns(&workspace).await.unwrap();
    let [turn] = turns.as_slice() else {
        panic!("one agent awaits a reply")
    };
    let agent = Caller::authenticated(
        root.verifying_key().to_bytes(),
        device.cert.device_public_key,
    )
    .in_workspace(workspace.clone())
    .delegated_to(SCOUT)
    .named("Scout")
    .in_agent_session("agent-session:proposal-review")
    .on_turn(
        tilo_actions::policy::turn::Turn::new(turn.conversation.clone(), turn.cause.clone()),
        turn.agent.bound_rules().unwrap_or_default(),
    );
    let initiative = server
        .roster
        .initiative
        .as_ref()
        .expect("the installed server answers initiative");
    let task = proposal(tidy(initiative, &workspace, &agent, "Tidy the meeting notes").await);
    for (verb, arguments) in [
        (
            workspace_initiative::ACCEPT,
            serde_json::json!({"proposal": task.id, "always": true}),
        ),
        (
            workspace_initiative::ASK_FIRST,
            serde_json::json!({"kind": task.kind}),
        ),
        (
            workspace_initiative::PROPOSE_AGAIN,
            serde_json::json!({"kind": task.kind}),
        ),
    ] {
        let error = take(initiative, &workspace, &agent, verb, arguments)
            .await
            .expect_err("the assistant cannot answer its own proposal for the person");
        assert!(error.to_string().contains("only the person"), "{error}");
    }
    let program = enrol(
        &root,
        "Ada's program",
        DeviceCapabilities::server_key_grant(),
    );
    let mut restricted =
        SignedPage::open(&server, &program, &host, &workspace, "assistant", REVIEW).await;
    let denied = restricted
        .until("the non-person page cannot read Ada's proposals", |html| {
            assert!(
                !html.contains(&task.title) && button(html, "Start task").is_none(),
                "the person's proposal was shown to a program"
            );
            (html.contains("The workspace could not complete the request")
                || html.contains("Open this workspace to review its proposals"))
            .then_some(())
        })
        .await;
    assert!(
        matches!(denied, Ok(()) | Err(PageRefusal::NotMember)),
        "the host or the initiative service refuses a program"
    );
    drop(restricted);
    let dm =
        ui::tilo_resource::items::item_resource_id::<WorkspaceAgents>("agents", &Id::from(SCOUT));
    let mut chat = SignedPage::open(&server, &device, &host, &workspace, "chat", &dm).await;
    let (app, anchor) = chat
        .until(
            "the signed person's DM links to their proposal review",
            |html| {
                button(html, "Review proposals")?;
                html.split('<').find_map(|part| {
                    let tag = part.split('>').next()?;
                    let attr =
                        |name: &str| tag.split_once(&format!(" {name}=\""))?.1.split('"').next();
                    let app = attr("data-open-app")?.strip_prefix("so.tilo.")?;
                    let item = attr("data-open-item")?;
                    (app == "assistant" && item == REVIEW)
                        .then(|| (app.to_owned(), item.to_owned()))
                })
            },
        )
        .await
        .unwrap();
    drop(chat);
    let open = || SignedPage::open(&server, &device, &host, &workspace, &app, &anchor);
    let mut review = open().await;

    persisted(
        &mut browser,
        "the signed device reads the proposal the agent just made",
        |state| state.proposals.iter().any(|p| p == &task),
    )
    .await;
    press(&mut review, "the proposed task and its reason", |html| {
        if !html.contains(&task.title)
            || !html.contains(&task.why)
            || button(html, "Start task").is_none()
        {
            return None;
        }
        button(html, "Always allow tasks of this kind")
    })
    .await;
    let confirm = review
        .until("the standing permission confirmation", |html| {
            button(html, "Confirm and start task")
        })
        .await
        .unwrap();
    persisted(
        &mut browser,
        "the first button leaves the task pending",
        |state| {
            state
                .proposals
                .iter()
                .any(|p| p.id == task.id && p.state == State::Proposed)
        },
    )
    .await;
    review.ask(&confirm).await;
    let ask_first = review
        .until(
            "a standing preference survives the last pending task",
            |html| button(html, "Ask first"),
        )
        .await
        .unwrap();
    persisted(
        &mut browser,
        "the signed decision entrusted this kind of task",
        |state| {
            state
                .kinds
                .iter()
                .any(|kind| kind.kind == task.kind && kind.always)
        },
    )
    .await;
    review.ask(&ask_first).await;
    persisted(
        &mut browser,
        "the task is accepted and future tasks need approval",
        |state| {
            state
                .proposals
                .iter()
                .any(|p| p.id == task.id && p.state == State::Accepted)
                && !state
                    .kinds
                    .iter()
                    .any(|kind| kind.kind == task.kind && kind.always)
        },
    )
    .await;

    for title in ["Tidy the weekly notes", "Tidy the planning notes"] {
        let task = proposal(tidy(initiative, &workspace, &agent, title).await);
        assert_eq!(task.state, State::Proposed);
        review = open().await;
        press(
            &mut review,
            "the person can decline this proposal",
            |html| {
                if !html.contains(title) {
                    return None;
                }
                button(html, "Decline")
            },
        )
        .await;
        persisted(&mut browser, "the person's decline was recorded", |state| {
            state
                .proposals
                .iter()
                .any(|p| p.id == task.id && p.state == State::Declined)
        })
        .await;
    }
    press(
        &mut review,
        "the person can resume a quiet kind without a pending task",
        |html| button(html, "Suggest again"),
    )
    .await;
    persisted(&mut browser, "the kind can be proposed again", |state| {
        !state
            .kinds
            .iter()
            .any(|kind| kind.kind == task.kind && (kind.quiet || kind.declined > 0))
    })
    .await;

    let event = proposal(
        take(
            initiative,
            &workspace,
            &agent,
            workspace_initiative::PROPOSE,
            serde_json::json!({
                "kind": "morning-brief",
                "title": "Morning meeting notes",
                "why": "The team meets each morning.",
                "event": {
                    "name": "Morning meeting notes",
                    "when": "every day at 09:00",
                    "onlyIf": "Is there a meeting today?",
                    "instruction": "Prepare the agenda."
                }
            }),
        )
        .await,
    );
    review = open().await;
    press(
        &mut review,
        "the event shows its schedule, condition and instruction",
        |html| {
            if ![
                event.title.as_str(),
                "every day at 09:00",
                "Is there a meeting today?",
                "Prepare the agenda.",
            ]
            .iter()
            .all(|text| html.contains(text))
            {
                return None;
            }
            assert!(
                button(html, "Always allow tasks of this kind").is_none(),
                "events require individual approval"
            );
            button(html, "Set up event")
        },
    )
    .await;
    persisted(
        &mut browser,
        "the person installed exactly the reviewed event",
        |state| {
            state
                .proposals
                .iter()
                .any(|p| p.id == event.id && p.state == State::Accepted && p.set_up.is_some())
        },
    )
    .await;
}
