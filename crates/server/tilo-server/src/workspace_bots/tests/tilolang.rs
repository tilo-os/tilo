//! A program over a tilolang app the server is built with: triggered by a person's change to
//! tilolang Tasks, it reads the workspace's tasks through the core read and adds a task through
//! Tasks' own published action, as the bot. A program over an app the workspace installed does
//! not run on the server.

use serde_json::json;
use tilo_actions::{ActionAddress, ActionContext, ActionInvoker, MemberId};
use tilo_automation::{ActorFilter, Binding, Selector, Shortcut, Step, StepKind, TriggerSpec};
use tilo_workspace_service::tl_records::{self, Opened, runtime::Actor};

use super::{installed, member_runner, membership, owner_root};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::governor;

const TASKS: &str = "so.tilo.lang.tasks";
const TASKS_KIND: &str = "tl.so.tilo.lang.tasks.Tasks";
/// The triager's installation.
const TRIAGER: &str = "3c9d2f61-8a4e-4b7c-9e15-2d6f0a8b7c43";

/// `shortcut` as the bot `installation` installed from `entry`, its grant what its steps and
/// triggers need.
fn installed_bot(installation: &str, shortcut: Shortcut) -> installed::InstalledBot {
    let shortcut = Shortcut {
        grant: tilo_automation::grant::derived_rules(
            &shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        ),
        ..shortcut
    };
    installed::InstalledBot {
        installation: installation.into(),
        entry: shortcut.id.clone(),
        name: shortcut.name.clone(),
        program: String::from_utf8(tilo_automation::program::canonical_bytes(&shortcut).unwrap())
            .unwrap(),
        inputs: serde_json::Map::new(),
        installed_at_ms: 1,
        release: String::new(),
    }
}

fn program(id: &str, trigger: TriggerSpec, steps: Vec<Step>) -> Shortcut {
    Shortcut {
        id: id.into(),
        name: id.into(),
        inputs: trigger.payload().to_vec(),
        steps,
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: vec![trigger],
        saved_by: None,
        saved_proof: None,
    }
}

/// A bot that answers each task a person adds: it reads what the workspace's Tasks holds and
/// adds a task to triage it.
fn triager() -> installed::InstalledBot {
    let trigger = TriggerSpec::ResourceChanged {
        kind: TASKS_KIND.into(),
        actions: vec!["new_task".into()],
        fields: Vec::new(),
        by: ActorFilter::People,
    };
    let steps = vec![
        Step::new(
            "look",
            StepKind::Read {
                app: TASKS.into(),
                record: String::new(),
                joined: false,
            },
        ),
        Step::new(
            "triage",
            StepKind::Invoke {
                action: tilo_actions::ActionKey::new(TASKS, TASKS_KIND, "new_task"),
                resource: Selector::FirstOfKind(TASKS_KIND.into()),
                args: [(
                    "title".to_owned(),
                    Binding::Literal("Triage the new task".into()),
                )]
                .into_iter()
                .collect(),
                appearance: None,
            },
        ),
    ];
    installed_bot(TRIAGER, program("triager", trigger, steps))
}

/// Add a task titled `title` to Tasks as `who`, through its published `new_task`, as their
/// window does.
fn new_task(tasks: &mut Opened, who: &str, title: &str) -> Result<(), String> {
    tasks.open_root()?;
    let root = tasks
        .rows()
        .into_iter()
        .find(|row| {
            row.action
                .as_ref()
                .is_some_and(|action| action.address.action == "new_task")
        })
        .ok_or("Tasks publishes new_task")?
        .resource
        .id;
    let arguments = json!({ "title": title });
    tasks
        .invoke_published(
            &ActionAddress::new(root, "new_task"),
            arguments.as_object().ok_or("arguments")?,
            &ActionInvoker::person(who),
            &ActionContext::default(),
        )
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The workspace's tasks as the core read pages them, read as `reader`.
async fn tasks(workspaces: &Workspaces, workspace: &str, reader: Actor) -> Vec<serde_json::Value> {
    let packages = tl_records::built_in(TASKS).unwrap().unwrap();
    workspaces
        .with_tl_record(workspace, &packages, reader.clone(), move |tasks| {
            let host = tasks.host();
            let documents = host.answer_read(&json!({}), &reader)?;
            let root = documents["records"]
                .as_array()
                .and_then(|records| records.iter().find(|record| record["kind"] == TASKS_KIND))
                .and_then(|record| record["item"].as_str())
                .ok_or("the workspace has its Tasks")?
                .to_owned();
            let page = host.answer_read(&json!({ "item": root, "field": "items" }), &reader)?;
            Ok(page["items"].as_array().cloned().unwrap_or_default())
        })
        .await
        .unwrap()
}

/// Each change of the workspace's Tasks, as its entry names it: the kind it wrote, what it did
/// and who did it.
async fn changes(workspaces: &Workspaces, workspace: &str) -> Vec<ActionContext> {
    let record = workspaces.tl_record(workspace, TASKS).await.unwrap();
    record
        .documents
        .values()
        .flat_map(|bytes| {
            let doc = automerge::Automerge::load(bytes).unwrap();
            doc.get_changes(&[])
                .into_iter()
                .filter_map(|change| ActionContext::from_change_message(change.message()?))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_program_answers_a_tasks_change_reading_and_acting_as_the_bot() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = super::bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    let owner = hex::encode(cert.user_public_key);
    let packages = tl_records::built_in(TASKS).unwrap().unwrap();
    let viewer = Actor::App(tl_records::runtime::ids::AppId::from(TASKS));
    let person = owner.clone();
    // A task from before the bot is installed, which its trigger must not answer.
    workspaces
        .with_tl_record(&workspace, &packages, viewer.clone(), move |tasks| {
            new_task(tasks, &person, "Water the plants")
        })
        .await
        .unwrap();

    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        triager(),
        scratch.path().join("triager.json"),
    )
    .await;
    // Installed now: what happened before is not answered.
    assert!(runner.step().await.unwrap().is_empty());

    let person = owner.clone();
    workspaces
        .with_tl_record(&workspace, &packages, viewer, move |tasks| {
            new_task(tasks, &person, "Call the landlord")
        })
        .await
        .unwrap();
    let reports = runner.step().await.unwrap();
    assert_eq!(
        reports.len(),
        1,
        "{reports:?}, not run: {:?}, the changes: {:?}, routes {:?}, docs {:?}",
        runner.dead_letters().collect::<Vec<_>>(),
        changes(&workspaces, &workspace).await,
        workspaces
            .workspace_document_routes(&workspace)
            .await
            .unwrap(),
        workspaces
            .tl_record(&workspace, TASKS)
            .await
            .unwrap()
            .documents
            .iter()
            .map(|(id, bytes)| {
                let doc = automerge::Automerge::load(bytes).unwrap();
                (
                    id.clone(),
                    doc.get_changes(&[])
                        .iter()
                        .map(|change| change.message().map(str::to_owned))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(
        reports[0].outcome,
        tilo_automation::InvocationOutcome::Completed,
        "{reports:?}"
    );
    let read = reports[0]
        .steps
        .iter()
        .find(|step| step.step == "look")
        .unwrap();
    let tilo_automation::invocation::StepStatus::Succeeded {
        output: Some(read), ..
    } = &read.status
    else {
        panic!("the read answered: {read:?}");
    };
    let documents = &read["records"][0]["value"]["records"];
    let tasks_root = documents
        .as_array()
        .and_then(|records| records.iter().find(|record| record["kind"] == TASKS_KIND))
        .unwrap_or_else(|| panic!("the core read lists the workspace's Tasks: {read}"));
    assert_eq!(
        tasks_root["created"]["who"]["actor"]["member"]["person"], owner,
        "the read says who made it: {tasks_root}"
    );
    // Its own task is a bot's, which the trigger does not count.
    assert!(runner.step().await.unwrap().is_empty());

    let bot = Actor::Member(MemberId::bot(TRIAGER));
    let listed = tasks(&workspaces, &workspace, bot.clone()).await;
    let made_by = |title: &str| {
        listed
            .iter()
            .filter(|task| task.to_string().contains(title))
            .map(|task| task["created"]["who"]["actor"]["member"].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(made_by("Call the landlord"), [json!({ "person": owner })]);
    assert_eq!(
        made_by("Triage the new task"),
        [json!({ "bot": TRIAGER })],
        "one task, the bot's: {listed:?}"
    );

    // The change itself names the bot, acting for the person who installed it, whose device
    // wrote it.
    let by_bot: Vec<_> = changes(&workspaces, &workspace)
        .await
        .into_iter()
        .filter_map(|context| context.invoker)
        .filter(|invoker| invoker.acting.by == bot)
        .collect();
    assert_eq!(by_bot.len(), 1, "{by_bot:?}");
    assert_eq!(by_bot[0].acting.acted_for_person(), Some(owner.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_program_over_an_installed_app_does_not_run_on_the_server() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = super::bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    let notes = "com.example.notes";
    let trigger = TriggerSpec::ResourceChanged {
        kind: format!("tl.{notes}.Note"),
        actions: Vec::new(),
        fields: Vec::new(),
        by: ActorFilter::People,
    };
    let bot = installed_bot(TRIAGER, program("noted", trigger, Vec::new()));
    let certifier = membership::HostRootCertifier::new(owner_root());
    let refused = installed::BotRunner::new(
        workspaces.clone(),
        workspace,
        cert,
        certifier.device_key(&bot.installation),
        bot,
        std::collections::BTreeMap::new(),
        scratch.path().join("noted.json"),
    )
    .err()
    .expect("the server does not run it");
    assert_eq!(refused, tl_records::runs_elsewhere(notes));
    assert!(
        refused.contains("runs only on a device that has it open"),
        "{refused}"
    );
}
