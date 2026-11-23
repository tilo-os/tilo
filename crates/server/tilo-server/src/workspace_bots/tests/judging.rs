//! The host's own answer to a judgement a person's program asks
//! ([`assistant::host_steps`]), through a program's real run: whose model is
//! asked, with whose key, where its cost is counted, and what a run is told
//! when no model answers.

use super::*;
use crate::workspaces::person_model::protocol::Content;
use crate::workspaces::person_model::scripted::{Asked, Linking, Reply, ScriptedProvider};
use crate::workspaces::person_model::{Interaction, ProviderProtocol};

/// The id of the assistant a test's person keeps.
pub(super) const ASSISTANT: &str = "assistant";
/// What the test's program asks its person's model.
const QUESTION: &str = "Is this the first working day of the month?";

/// `person` keeps an assistant in `workspace`, as their own device makes one:
/// an agent in the workspace's record of its agents, with the most it may
/// spend in a day.
pub(super) async fn keeps_an_assistant(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    acts_for: &str,
    limit_micros: Option<u64>,
) {
    use tilo_workspace_agent::{WorkspaceAgents, workspace_agents};
    assert!(
        cert == workspaces.governor(),
        "the server writes the record as its governor"
    );
    workspaces
        .author_agents(workspace, |doc| {
            let mut made = tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                .as_author(acts_for);
            made.apply(
                WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                    name: tilo_resource::NonBlank::parse("Assistant").unwrap(),
                    id: Some(ASSISTANT.into()),
                    model: None,
                    mcp_client: None,
                }),
            )
            .unwrap();
            made.apply(WorkspaceAgents::SET_AGENT_SPEND_LIMIT.with(
                workspace_agents::SetAgentSpendLimit {
                    agent: tilo_resource::Id::from_raw(ASSISTANT.to_owned()),
                    limit_micros,
                },
            ))
            .unwrap();
            std::mem::swap(doc, made.doc_mut());
            Ok(())
        })
        .await
        .unwrap();
}

/// A person's own program: at `at_ms`, ask their model [`QUESTION`] about
/// the moment, and keep what it answered for the next run.
fn keeping_what_was_judged(at_ms: i64) -> installed::InstalledBot {
    use tilo_automation::{Binding, Step, StepKind};
    at_a_moment(
        at_ms,
        vec![
            Step::new(
                "judge",
                StepKind::Ai {
                    prompt: QUESTION.into(),
                    output: Some(serde_json::json!({"type": "boolean"})),
                    input: Some(Binding::Template("{{input.event}}".into())),
                },
            ),
            own_step(
                "keep",
                "keep",
                vec![(
                    "kept",
                    Binding::Object(
                        [("due".to_owned(), Binding::step("judge", ""))]
                            .into_iter()
                            .collect(),
                    ),
                )],
            ),
        ],
    )
}

/// The provider `workspace` keeps for itself, with its own key, choosing its
/// small model for judging; it answers as `replies` script it.
async fn their_own_model(
    workspaces: &Workspaces,
    workspace: &str,
    replies: Vec<Reply>,
) -> std::sync::Arc<ScriptedProvider> {
    let theirs = ScriptedProvider::answering(replies);
    workspaces.answer_models_with(theirs.clone());
    let provider = workspaces
        .link_model_for(
            workspace,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-their-own"),
                models: &["gemini-main", "gemini-small"],
            },
            true,
        )
        .await
        .unwrap();
    workspaces
        .choose_model_for(
            workspace,
            Interaction::Classifier,
            &provider,
            "gemini-small",
        )
        .unwrap();
    theirs
}

/// The program [`keeping_what_was_judged`] at `at_ms`, run once for
/// `person` with the host's own answers: its runner after the pass, and the
/// pass's reports.
async fn judged_once(
    (workspaces, workspace, cert): (&Workspaces, &str, &DeviceCert),
    person: &str,
    at_ms: i64,
    state: std::path::PathBuf,
) -> (installed::BotRunner, Vec<tilo_automation::InvocationReport>) {
    let mut runner = member_runner(
        workspaces,
        workspace,
        cert,
        keeping_what_was_judged(at_ms),
        state,
    )
    .await
    .runs_for(person.to_owned(), assistant::host_steps(workspaces.clone()));
    runner.use_clock(std::sync::Arc::new(tilo_clock::ManualClock::new(
        u64::try_from(at_ms).unwrap(),
    )));
    let reports = runner.step().await.unwrap();
    (runner, reports)
}

/// What message `message` of `call` says in words.
fn said(call: &Asked, message: usize) -> String {
    call.request
        .messages
        .get(message)
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|part| match part {
                    Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The host's own answer to a judgement: the model the workspace keeps for
/// judging, asked with the key it keeps, and held to the shape the step asked
/// for.
#[tokio::test(flavor = "multi_thread")]
async fn a_judgement_is_answered_by_its_workspaces_own_model_with_its_own_key() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server_taking_part(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let at_ms = i64::try_from(FIRST_TICK_MS).unwrap();
    // The person the workspace is of: an assistant lives in a workspace of
    // its person's own.
    let person = cert.user_public_key;
    let theirs = their_own_model(
        &workspaces,
        &workspace,
        vec![
            Reply::text("It is.").costing(100),
            Reply::text(r#"{"result": true}"#).costing(150),
        ],
    )
    .await;

    let (runner, reports) = judged_once(
        (&workspaces, &workspace, &cert),
        &hex::encode(person),
        at_ms,
        scratch.path().join("judged.json"),
    )
    .await;
    assert_eq!(
        reports.len(),
        1,
        "{reports:?}, not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}"
    );
    assert_eq!(
        runner.kept().read().note().kept,
        serde_json::json!({"due": true}),
        "the run goes on with what their model answered"
    );
    let asked = theirs.asked();
    assert_eq!(
        asked.len(),
        2,
        "an answer that did not fit its shape is asked again once: {asked:?}"
    );
    for call in &asked {
        assert_eq!(
            call.model, "gemini-small",
            "the model the workspace chose for judging"
        );
        assert_eq!(
            call.key.as_deref(),
            Some("sk-their-own"),
            "with the workspace's own key"
        );
        assert!(call.request.tools.is_empty(), "a judgement acts on nothing");
        assert!(said(call, 0).contains(QUESTION), "{}", said(call, 0));
        assert!(
            said(call, 1).contains(&at_ms.to_string()),
            "it judges what fired the run: {}",
            said(call, 1)
        );
    }
}

/// What a judgement costs is counted on the day of the assistant its person
/// keeps, and a refusal is passed on in the words it has for them.
#[tokio::test(flavor = "multi_thread")]
async fn a_judgements_cost_is_on_its_persons_assistants_day_and_a_refusal_is_said_in_words() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server_taking_part(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let (workspace, _) = chat_workspace(&workspaces, &cert).await;
    let at_ms = i64::try_from(FIRST_TICK_MS).unwrap();
    let person = cert.user_public_key;
    let owner = hex::encode(person);
    // The workspace's provider charges 250 for the one answer, and their
    // assistant may spend 250 in a day.
    let theirs = their_own_model(
        &workspaces,
        &workspace,
        vec![Reply::text(r#"{"result": true}"#).costing(250)],
    )
    .await;
    keeps_an_assistant(&workspaces, &workspace, &cert, &owner, Some(250)).await;
    let set = (&workspaces, workspace.as_str(), &cert);

    let (runner, reports) =
        judged_once(set, &owner, at_ms, scratch.path().join("first.json")).await;
    assert_eq!(
        reports.len(),
        1,
        "{reports:?}, not run: {:?}",
        runner.dead_letters().collect::<Vec<_>>()
    );
    assert_eq!(theirs.asked().len(), 1);
    drop(runner);

    // The 250 are on their assistant's day: its limit is used, so the next
    // judgement is refused before their provider is asked, in the words
    // their settings have for it.
    let (runner, _) = judged_once(
        set,
        &owner,
        at_ms.saturating_add(60_000),
        scratch.path().join("limit.json"),
    )
    .await;
    let dead: Vec<_> = runner.dead_letters().collect();
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].step.as_deref(), Some("judge"));
    assert!(dead[0].reason.contains("has used today's"), "{dead:?}");
    assert_eq!(theirs.asked().len(), 1, "nothing is asked past the limit");
    assert_eq!(runner.kept().read().note().kept, Value::Null);
    drop(runner);

    // A workspace with no model linked tells its program so. Another
    // workspace's model answers for it no more than a person's did, and the
    // run guesses nothing.
    let (elsewhere, _) = chat_workspace(&workspaces, &cert).await;
    let (runner, _) = judged_once(
        (&workspaces, elsewhere.as_str(), &cert),
        &hex::encode(MALLORY),
        at_ms.saturating_add(120_000),
        scratch.path().join("none.json"),
    )
    .await;
    let dead: Vec<_> = runner.dead_letters().collect();
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].step.as_deref(), Some("judge"));
    assert!(
        dead[0]
            .reason
            .contains("No model is linked for this kind of work"),
        "{dead:?}"
    );
    assert_eq!(
        theirs.asked().len(),
        1,
        "no other workspace's model is asked"
    );
    assert_eq!(runner.kept().read().note().kept, Value::Null);
}
