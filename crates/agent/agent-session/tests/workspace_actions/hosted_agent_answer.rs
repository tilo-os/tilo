//! The workspace the agent-answer tests work in, and a model that calls tools before it answers.
use super::*;

/// A workspace on a server with one person in its chat and their agent kept there. The person's
/// page joins: its device is the one their agent's turns are admitted on.
macro_rules! team {
    ($root:ident, $browser:ident, $actions:ident, $workspaces:ident, $workspace:ident, $page:ident, $served:ident) => {
        let ($root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let server = enrol(&$root, "tilo-sh", DeviceCapabilities::member());
        let $browser = enrol(&$root, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let $served = crate::harness::hosted::start(
            &$root,
            &server,
            state.path(),
            crate::harness::hosted::Options::offering(crate::common::catalogue),
        )
        .await;
        let $actions = $served.roster.actions.clone();
        let $workspaces = $served.workspaces().unwrap().clone();
        let $workspace = $workspaces.create("Team").await.unwrap().workspace;
        let $page = Browser::open_at(&$served, &$browser, &server, &$workspace).await;
        let person = hex::encode($root.verifying_key().to_bytes());
        make_agent(&$page, &$workspaces, &$workspace, &person, None).await;
        let catalogue = crate::common::catalogue();
        let governor = $workspaces.device_cert().clone();
        $workspaces
            .prepare_application(&$workspace, &governor, &catalogue, CHAT, "")
            .await
            .unwrap();
    };
}
pub(super) use team;

/// Answer the one turn the workspace owes, with the model `$model`.
macro_rules! answer_the_turn {
    ($served:ident, $workspace:ident, $model:expr) => {
        let agents = $served
            .approving(&$workspace, &[SCOUT])
            .await
            .scripted($model);
        let turns = agents.turns(&$workspace).await.unwrap();
        assert_eq!(turns.len(), 1, "the message is the agent's turn: {turns:?}");
        tokio::time::timeout(
            Duration::from_secs(90),
            agents.answer(&$workspace, &turns[0]),
        )
        .await
        .expect("the turn ends")
        .unwrap();
    };
}
pub(super) use answer_the_turn;

/// A model that makes each of `calls` (a tool and what it is given), one a turn, then answers
/// `words`.
pub(super) fn calls_then_answers(
    calls: Vec<(&'static str, serde_json::Value)>,
    words: &'static str,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
) -> Model {
    Arc::new(move || {
        let mut answers: Vec<ModelResponse> = calls
            .iter()
            .enumerate()
            .map(|(n, (tool, input))| {
                answer(
                    vec![Content::ToolCall(ToolCall {
                        id: format!("call-{n}"),
                        name: (*tool).into(),
                        input: input.clone(),
                        extra_content: None,
                    })],
                    StopReason::ToolCalls,
                    1_000,
                )
            })
            .collect();
        answers.push(answer(
            vec![Content::Text { text: words.into() }],
            StopReason::EndTurn,
            1_000,
        ));
        Some(Box::new(Scripted {
            answers,
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// What `request` tells the model its tools answered, each with whether it failed.
pub(super) fn tool_answers(request: &ModelRequest) -> Vec<(String, bool)> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some((result.content.clone(), result.is_error)),
            _ => None,
        })
        .collect()
}
