//! A workspace's skills say how work is done there. Chat keeps them, its people write them
//! through Chat's own actions, and a hosted agent's turn is told they exist and reads one,
//! with the examples it keeps, through the tools it has, as the person it works for. Nothing of
//! this is per agent: the same actions write and read the skills a person edits.
use super::*;
use tilo_server::workspaces::Workspaces;
use tilo_workspace_service::tl_records::runtime::Value;

/// What a person writes down about how the weekly report is done, and a result of it they
/// rejected.
const NAME: &str = "Weekly report";
const WORK: &str = "Answering a request that asks for a weekly report";
const HOW: &str = "Put the weekly numbers first.";
const REJECTED: &str = "The launch slipped by a day; say so first.";
const SAID: &str = "Too vague";
/// A result the person accepted afterwards.
const ACCEPTED: &str = "Revenue rose 4 %, churn fell.";

/// An example as the test reads it: the result, whether the person accepted it, and what they
/// said about it.
type ExampleRow = (String, bool, String);

/// The workspace's skills, each by its name and how its work is done, with its examples, as
/// Chat keeps them.
async fn skills_kept(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
) -> Vec<(String, String, Vec<ExampleRow>)> {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    workspaces
        .with_tl_record(workspace, &packages, who, move |opened| {
            let all = opened
                .host()
                .root_of(&chat)
                .map_err(|error| error.to_string())?;
            let read = |at: &asks::ResourceRef, field: &str| {
                opened
                    .host()
                    .runtime()
                    .read(at, field)
                    .map_err(|error| error.to_string())
            };
            let Value::List(skills) = read(&all, "skills")? else {
                return Err("skills read as no list".to_owned());
            };
            let mut kept = Vec::new();
            for skill in skills {
                let Value::Ref(skill) = skill else {
                    return Err("a skill is no resource".to_owned());
                };
                let skill = (*skill.target).clone();
                let Value::Text(name) = read(&skill, "name")? else {
                    return Err("a skill's name reads as no text".to_owned());
                };
                let Value::Text(how) = read(&skill, "how")? else {
                    return Err("a skill's words read as no text".to_owned());
                };
                let Value::List(examples) = read(&skill, "examples")? else {
                    return Err("examples read as no list".to_owned());
                };
                let mut kept_examples = Vec::new();
                for example in examples {
                    let Value::Ref(example) = example else {
                        return Err("an example is no resource".to_owned());
                    };
                    let example = (*example.target).clone();
                    let (Value::Text(text), Value::Bool(accepted), Value::Text(note)) = (
                        read(&example, "text")?,
                        read(&example, "accepted")?,
                        read(&example, "note")?,
                    ) else {
                        return Err("an example's words read as no text".to_owned());
                    };
                    kept_examples.push((text, accepted, note));
                }
                kept.push((name, how, kept_examples));
            }
            Ok(kept)
        })
        .await
        .unwrap()
}

/// The skill called `name` in Chat's record, as `who` opens it.
fn skill_named(
    opened: &tilo_workspace_service::tl_records::Opened,
    chat: &str,
    name: &str,
) -> Result<Value, String> {
    let all = opened
        .host()
        .root_of(chat)
        .map_err(|error| error.to_string())?;
    let Value::List(skills) = opened
        .host()
        .runtime()
        .read(&all, "skills")
        .map_err(|error| error.to_string())?
    else {
        return Err("skills read as no list".to_owned());
    };
    for skill in skills {
        let Value::Ref(skill) = skill else {
            continue;
        };
        let skill = (*skill.target).clone();
        let Value::Text(named) = opened
            .host()
            .runtime()
            .read(&skill, "name")
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        if named == name {
            return Ok(Value::resource(&skill));
        }
    }
    Err(format!("no skill is called {name}"))
}

/// The person starts a skill through Chat's action, as a device with no window open does it.
async fn starts_skill(workspaces: &Workspaces, workspace: &str, person: &str, name: &str) {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    let name = name.to_owned();
    workspaces
        .with_tl_record(workspace, &packages, who.clone(), move |opened| {
            let all = opened
                .host()
                .root_of(&chat)
                .map_err(|error| error.to_string())?;
            opened
                .invoke(
                    &who,
                    (&chat, "new_skill"),
                    vec![Value::resource(&all), Value::Text(name)],
                    None,
                )
                .map(drop)
        })
        .await
        .unwrap();
}

/// The person writes a skill down through Chat's actions: started, then corrected with the work
/// it is for and how that work is done here.
async fn writes_skill(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    (name, work, how): (&str, &str, &str),
) {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    let (name, work, how) = (name.to_owned(), work.to_owned(), how.to_owned());
    workspaces
        .with_tl_record(workspace, &packages, who.clone(), move |opened| {
            let all = opened
                .host()
                .root_of(&chat)
                .map_err(|error| error.to_string())?;
            let made = opened.invoke(
                &who,
                (&chat, "new_skill"),
                vec![Value::resource(&all), Value::Text(name.clone())],
                None,
            )?;
            opened
                .invoke(
                    &who,
                    (&chat, "correct_skill"),
                    vec![
                        made,
                        Value::Text(name),
                        Value::Text(work),
                        Value::Text(how),
                        Value::Text(String::new()),
                        Value::Text(String::new()),
                    ],
                    None,
                )
                .map(drop)
        })
        .await
        .unwrap();
}

/// The person keeps an example of the skill `skill` through Chat's action: a result they
/// accepted or rejected, with what they said about it.
async fn keeps_example(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    (skill, text, accepted, said): (&str, &str, bool, &str),
) {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    let (skill, text, said) = (skill.to_owned(), text.to_owned(), said.to_owned());
    workspaces
        .with_tl_record(workspace, &packages, who.clone(), move |opened| {
            let skill = skill_named(opened, &chat, &skill)?;
            opened
                .invoke(
                    &who,
                    (&chat, "keep_example"),
                    vec![
                        skill,
                        Value::Text(text),
                        Value::Bool(accepted),
                        Value::Text(said),
                    ],
                    None,
                )
                .map(drop)
        })
        .await
        .unwrap();
}

/// A model whose answer to each call is `play`'s. Every call is kept, in the order asked.
struct Plays {
    play: Arc<dyn Fn(&ModelRequest) -> ModelResponse + Send + Sync>,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl tilo_agent_session::model::PlanTurns for Plays {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        Ok((self.play)(request))
    }

    fn model(&self) -> &'static str {
        "plays"
    }
}

fn plays(
    asked: &Arc<Mutex<Vec<ModelRequest>>>,
    play: impl Fn(&ModelRequest) -> ModelResponse + Send + Sync + 'static,
) -> Model {
    let asked = Arc::clone(asked);
    let play: Arc<dyn Fn(&ModelRequest) -> ModelResponse + Send + Sync> = Arc::new(play);
    Arc::new(move || {
        Some(Box::new(Plays {
            play: Arc::clone(&play),
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// A model turn that calls `name` with `input`, as the turn's tools take it.
fn calls(id: &str, name: &str, input: serde_json::Value) -> ModelResponse {
    answer(
        vec![Content::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            extra_content: None,
        })],
        StopReason::ToolCalls,
        1_000,
    )
}

/// A model turn that says `said` and ends.
fn says(said: &str) -> ModelResponse {
    answer(
        vec![Content::Text { text: said.into() }],
        StopReason::EndTurn,
        1_000,
    )
}

/// What the tools answered in `request`, in the order asked: each answer's words, and whether
/// it was a failure.
fn tool_answers(request: &ModelRequest) -> Vec<(String, bool)> {
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

/// The path a find answered with a resource of Chat's: the first line naming one.
fn path_of(answered: &str) -> String {
    answered
        .lines()
        .find(|line| line.contains("tl.so.tilo.chat"))
        .and_then(|line| line.split("  ").next())
        .unwrap_or_else(|| panic!("no resource of Chat's in what the find answered: {answered}"))
        .trim()
        .to_owned()
}

/// A hosted workspace with one person and their agent working on the server. The person writes
/// down how the weekly report is done and keeps a result they rejected as an example, then asks
/// the agent for it. The turn is told the workspace has skills and that they keep examples; it
/// finds the skill and the example and reads both. The person then adds another skill and keeps
/// a result they accepted, both through the same actions.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_skill_is_told_read_and_written_through_chats_actions() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let server = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let served = crate::harness::hosted::start(
        &root,
        &server,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let page = Browser::open_at(&served, &browser, &server, &workspace).await;
    let person = hex::encode(root.verifying_key().to_bytes());
    make_agent(&page, &workspaces, &workspace, &person, None).await;

    writes_skill(&workspaces, &workspace, &person, (NAME, WORK, HOW)).await;
    keeps_example(
        &workspaces,
        &workspace,
        &person,
        (NAME, REJECTED, false, SAID),
    )
    .await;
    assert_eq!(
        skills_kept(&workspaces, &workspace, &person).await,
        [(
            NAME.to_owned(),
            HOW.to_owned(),
            [(REJECTED.to_owned(), false, SAID.to_owned())].to_vec(),
        )]
    );

    // The person asks the agent for the weekly report, and the model answers as a turn that
    // read the workspace's skill and its example.
    write_to_agent(
        &served.roster.actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Write this week's report.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let model = plays(&asked, |request| {
        let answers = tool_answers(request);
        match answers.len() {
            0 => {
                let told = said_in(request);
                assert!(
                    told.contains("workspace skills"),
                    "the turn is told the workspace keeps skills: {told}"
                );
                assert!(
                    told.contains("examples"),
                    "and that its skills keep examples: {told}"
                );
                calls(
                    "find-skill",
                    "tilo_find",
                    serde_json::json!({"query": NAME}),
                )
            }
            1 => {
                assert!(!answers[0].1, "the skill is found: {:?}", answers[0]);
                let path = path_of(&answers[0].0);
                calls("read-skill", "tilo_read", serde_json::json!({"path": path}))
            }
            2 => {
                assert!(!answers[1].1, "the skill reads: {:?}", answers[1]);
                assert!(
                    answers[1].0.contains(HOW),
                    "the skill's words: {}",
                    answers[1].0
                );
                calls(
                    "find-example",
                    "tilo_find",
                    serde_json::json!({"query": "slipped"}),
                )
            }
            3 => {
                assert!(!answers[2].1, "the example is found: {:?}", answers[2]);
                let path = path_of(&answers[2].0);
                calls(
                    "read-example",
                    "tilo_read",
                    serde_json::json!({"path": path}),
                )
            }
            _ => {
                let (read, failed) = answers.last().unwrap().clone();
                assert!(!failed, "the example reads: {read}");
                assert!(read.contains(REJECTED), "the example's result: {read}");
                assert!(read.contains(SAID), "what the person said: {read}");
                says("I wrote it, with the weekly numbers first.")
            }
        }
    });
    let agents = served.approving(&workspace, &[SCOUT]).await.scripted(model);
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "the ask is the agent's turn: {turns:?}");
    tokio::time::timeout(
        Duration::from_secs(90),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();

    // The person adds another skill through the same action, and keeps a result they accepted
    // with the first: both are the workspace's.
    starts_skill(&workspaces, &workspace, &person, "Meeting recap").await;
    keeps_example(&workspaces, &workspace, &person, (NAME, ACCEPTED, true, "")).await;
    assert_eq!(
        skills_kept(&workspaces, &workspace, &person).await,
        [
            (
                NAME.to_owned(),
                HOW.to_owned(),
                [
                    (REJECTED.to_owned(), false, SAID.to_owned()),
                    (ACCEPTED.to_owned(), true, String::new()),
                ]
                .to_vec(),
            ),
            ("Meeting recap".to_owned(), String::new(), Vec::new()),
        ]
    );
    page.peer.close();
}
