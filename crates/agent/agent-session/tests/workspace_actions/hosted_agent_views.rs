//! A turn of an agent on a workspace's server answers with a view it wrote: on its reply to the
//! ask, as the agent. Which reply it goes with and whose view it is are never the model's to
//! say, and a reply that ended takes no more.
use super::*;

/// A view over what Tasks keeps for the workspace, as a model writes one.
const PROJECTS: &str =
    "view projects() {\n    for a in tasks.tracker.areas {\n        text(a.name)\n    }\n}";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_a_server_answers_with_a_view_it_wrote() {
    team!(root, browser, actions, workspaces, workspace, page);
    asks_scout(&workspaces, &workspace, &root, "Which projects are there?").await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(
        workspaces,
        actions,
        workspace,
        calls_then_answers(
            vec![(
                "answer_with_view",
                serde_json::json!({"title": "Projects", "source": PROJECTS}),
            )],
            "Your projects, as Tasks keeps them.",
            Arc::clone(&asked),
        )
    );

    // The view is the agent's own reply's, beside its words: one answer.
    let reply = reply(&workspaces, &workspace, &root).await;
    assert_eq!(reply.state, AskState::Done, "{reply:?}");
    assert_eq!(
        reply.views,
        [("Projects".to_owned(), PROJECTS.to_owned())],
        "the reply carries the view the model wrote"
    );
    assert_eq!(reply.body, "Your projects, as Tasks keeps them.");

    // The model was offered the tool with how a view is written and when one is the answer,
    // and was told the view went with its reply before it said what it shows.
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "one turn to write the view, one to answer");
    let tool = asked[0]
        .tools
        .iter()
        .find(|tool| tool.name == "answer_with_view")
        .expect("a turn that answers an ask is offered the tool");
    assert!(
        tool.description.contains("tasks.tracker") && tool.description.contains("keeps changing"),
        "{}",
        tool.description
    );
    assert!(
        said_in(&asked[0]).contains("answer_with_view"),
        "its instructions say when a view is the answer"
    );
    let told = told(&asked);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        !told[0].1 && told[0].0.contains("goes with your reply"),
        "the tool said the view is on the reply: {told:?}"
    );

    // The reply ended: the agent shows nothing more on it, neither a view nor a card.
    let agent = MemberId::agent(SCOUT);
    let packages = asks::chat_packages().unwrap();
    let at = reply.at.clone();
    let refused = workspaces
        .with_tl_record(
            &workspace,
            &packages,
            asks::Actor::Member(agent.clone()),
            move |opened| {
                let view = asks::show_view(opened, &agent, &at, ("Again", PROJECTS));
                let card = asks::show(
                    opened,
                    &agent,
                    &at,
                    (
                        "A poem",
                        &Reference::resource(WRITER, "tilo.window", WRITER),
                    ),
                );
                Ok((view, card))
            },
        )
        .await
        .unwrap();
    for refused in [refused.0, refused.1] {
        assert!(
            refused
                .as_ref()
                .is_err_and(|why| why.contains("The answer is finished")),
            "a finished reply takes nothing more: {refused:?}"
        );
    }
    let still = super::reply(&workspaces, &workspace, &root).await;
    assert_eq!(still.views.len(), 1, "{still:?}");
    assert!(still.shown.is_empty(), "{still:?}");
    page.peer.close();
}

/// Each call is one view the model may not post: one that says whose it is, one that names the
/// reply it goes with, one that shows nothing, and one with no source at all. Each is refused and
/// told to the model as a failure, and the reply carries no view.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_view_a_model_may_not_post_is_refused_and_told_to_it() {
    team!(root, browser, actions, workspaces, workspace, page);
    asks_scout(&workspaces, &workspace, &root, "Which projects are there?").await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let refused: Vec<(serde_json::Value, &str)> = vec![
        (
            serde_json::json!({"source": PROJECTS, "author": format!("person:{person}")}),
            "never arguments",
        ),
        (
            serde_json::json!({"source": PROJECTS, "reply": "elsewhere"}),
            "never arguments",
        ),
        (
            serde_json::json!({"title": "Projects", "source": " "}),
            "takes `source`",
        ),
        (serde_json::json!({"title": "Projects"}), "takes `source`"),
    ];
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(
        workspaces,
        actions,
        workspace,
        calls_then_answers(
            refused
                .iter()
                .map(|(input, _)| ("answer_with_view", input.clone()))
                .collect(),
            "I could not show that as a view.",
            Arc::clone(&asked),
        )
    );

    let reply = reply(&workspaces, &workspace, &root).await;
    assert!(
        reply.views.is_empty(),
        "no refused view was posted: {reply:?}"
    );
    assert_eq!(reply.body, "I could not show that as a view.");
    // Taken out whole: no lock is held while the test asks the server anything more.
    let asked = asked.lock().unwrap().clone();
    let told = told(&asked);
    assert_eq!(told.len(), refused.len(), "{told:?}");
    for ((answer, failed), (input, why)) in told.iter().zip(&refused) {
        assert!(*failed, "{input} is refused as a failure: {answer}");
        assert!(
            answer.contains(why),
            "{input} is refused for {why:?}: {answer}"
        );
    }
    page.peer.close();
}
