//! `tilo migrate` as a person runs it, through the device's daemon: the steps are the ones the
//! command takes ([`tilo_migrate::provider::caller::Steps`]), each an invocation of the device's
//! migrations followed to its end, and the question a run asks its person (keeping a Thunderbird
//! mail account) is the asked record the workspace lists, answered with its own `resolve`, as the
//! terminal answers it. `-w` names the workspace the migration lands in: another the device is a
//! member of takes the run there, as the migration's agent in that workspace, and one it is not
//! in is refused naming the command that joins it.
//!
//! Nothing stands in but the mail server's sign-in: the daemon serves its own migrations over a
//! Thunderbird profile in the test's home, the run keeps the account through the workspace's
//! `workspace/mail-accounts#add` as the device's migration agent, and the workspace's policy
//! asks the person first.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_actions::remote::CatalogueQuery;
use tilo_actions::remote::asked::Asked;
use tilo_migrate::provider::caller::{Answering, Asks, Steps, question_said};
use tilo_migrate::thunderbird::fixture::WORK_EMAIL;

use crate::language::agent_fixture::said;
use crate::language::host;
use crate::language::migrate_window::{agent_does, engine};

/// The Migrations app, whose own records a step's review is kept in.
const MIGRATE: &str = "so.tilo.migrate";

/// What a step answered, and every line it said on the way, the questions asked included.
struct Stepped {
    answer: Value,
    said: Vec<String>,
}

/// The step `action` of the device's migrations with `args`, as `tilo migrate` takes it,
/// answering the questions its run asks as `asks` says.
async fn step(steps: &Arc<Steps>, action: &str, args: Value, asks: Asks) -> Stepped {
    let Value::Object(args) = args else {
        panic!("a step's arguments are an object")
    };
    let said = Arc::new(Mutex::new(Vec::new()));
    let telling = Arc::clone(&said);
    let mut say = move |line: &str| telling.lock().unwrap().push(line.to_owned());
    let mut prompt = |question: &Asked| -> anyhow::Result<bool> {
        panic!("nobody is at a terminal here: {}", question_said(question))
    };
    let stepped = match steps
        .step(
            action,
            args,
            &mut Answering {
                asks,
                prompt: &mut prompt,
                say: &mut say,
            },
        )
        .await
    {
        Ok(stepped) => stepped,
        Err(error) => panic!("{action} reaches the device: {error:#}"),
    };
    let answer = stepped.unwrap_or_else(|failure| panic!("{action} runs: {failure:?}"));
    let said = said.lock().unwrap().clone();
    Stepped { answer, said }
}

/// The step `action` with `args`, which the device refuses before it runs: what it says.
async fn refused(steps: &Arc<Steps>, action: &str, args: Value) -> String {
    let Value::Object(args) = args else {
        panic!("a step's arguments are an object")
    };
    let mut say = |_: &str| {};
    let mut prompt = |question: &Asked| -> anyhow::Result<bool> {
        panic!("a refused step asks nothing: {}", question_said(question))
    };
    let answered = steps
        .step(
            action,
            args,
            &mut Answering {
                asks: Asks::Leave,
                prompt: &mut prompt,
                say: &mut say,
            },
        )
        .await;
    match answered {
        Err(error) => format!("{error:#}"),
        Ok(ran) => panic!("{action} ran: {ran:?}"),
    }
}

/// What `tilo_ls` of `path` lists in `workspace`, as the person's agent reads it there.
async fn listed_in(agent: &host::Agent, workspace: &str, path: &str) -> String {
    let (text, failed) = said(
        &agent
            .tool("tilo_ls", &json!({"workspace": workspace, "path": path}))
            .await,
    );
    assert!(!failed, "{path} lists in {workspace}: {text}");
    text
}

/// A workspace of the person's made now through their account, which this device is a member
/// of: its id.
async fn made(host: &host::Host, name: &str) -> String {
    let (status, answer) = host
        .key("Migration walk")
        .post(
            "/v1/resources/account/workspaces/actions/create",
            &json!({"name": name}),
        )
        .await;
    assert_eq!(status, 200, "the account makes a workspace: {answer}");
    answer["result"]["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a create answers the workspace it made: {answer}"))
        .to_owned()
}

/// The addresses of the mail accounts the workspace keeps, as the migration reads them.
async fn kept_accounts(agent: &host::Agent) -> Vec<String> {
    let listed = agent_does(agent, "workspace/mail-accounts", "list", &json!({}))
        .await
        .expect("the workspace lists its mail accounts");
    listed["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|account| account["address"].as_str().map(str::to_owned))
        .collect()
}

/// The device's key, as its migrations' address names it (`devices/<key>/migrate`).
fn device_of(engine: &str) -> [u8; 32] {
    let hex = engine
        .strip_prefix("devices/")
        .and_then(|rest| rest.strip_suffix("/migrate"))
        .unwrap_or_else(|| panic!("a device's migrations: {engine}"));
    hex::decode(hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .unwrap_or_else(|| panic!("a device key: {hex}"))
}

/// A Thunderbird migration whose chosen mail account its person is asked about: with nobody to
/// ask the terminal says no, the account stays unconnected and the run says so; `--yes` keeps it
/// in the workspace's mail accounts. The question names the migration's agent and the account.
/// `-w` naming a workspace the device is not in is refused with the command that joins it; one
/// it is a member of takes the run there, and what it brings is that workspace's, not the
/// device's own.
#[test]
fn the_terminal_walks_a_thunderbird_migration_on_the_device() {
    host::run_with(&["migrate", "contacts"], |host| async move {
        let home = PathBuf::from(std::env::var_os("HOME").expect("the test's home"));
        tilo_migrate::thunderbird::fixture::write_sample_profile(&home);
        // The mail server is the one thing stood in for: it takes the password Thunderbird saved.
        tilo_person_services::PersonServices::of(host.daemon().workspaces())
            .sign_in_accounts_through(|_, password| {
                if password == tilo_migrate::thunderbird::fixture::WORK_PASSWORD {
                    Ok("signed in".into())
                } else {
                    Err("IMAP server rejected the credential".into())
                }
            });
        // Migrate keeps its own state in the workspace it is installed in: the person's own,
        // as a person who installs it there makes it.
        host.install_app(MIGRATE).await;
        host.daemon().serve_migrations();
        let device = device_of(&engine(host.as_ref()));
        // The terminal reaches the daemon on the person's own connection, which the kernel says
        // is theirs: what their CLI's prompt answers, and what leaves their devices, is theirs.
        let (own, caller) = host
            .daemon()
            .own_connection_in(host.workspace())
            .expect("the person's own connection reaches the workspace");
        let steps_in = |named: &str| {
            let mut steps = Steps::new(
                Arc::clone(&own),
                caller.clone(),
                &device,
                host.workspace().to_owned(),
                None,
            );
            steps
                .in_workspace(named)
                .unwrap_or_else(|error| panic!("-w {named}: {error:#}"));
            Arc::new(steps)
        };
        let agent = host.agent();

        // `-w` names a workspace: one the device is not in is refused, naming how to join it,
        // by the device when it is named by an id and here when by a name it does not know.
        let elsewhere = "00000000-0000-4000-8000-000000000000";
        let refusal = refused(
            &steps_in(elsewhere),
            "review",
            json!({"source": "thunderbird"}),
        )
        .await;
        assert!(refusal.contains("tilo login"), "{refusal}");
        assert!(refusal.contains(elsewhere), "{refusal}");
        let mut unknown = Steps::new(
            host.dispatch(),
            host.caller(),
            &device,
            host.workspace().to_owned(),
            None,
        );
        let refusal = format!(
            "{:#}",
            unknown
                .in_workspace("Lab")
                .expect_err("the device knows no workspace by that name")
        );
        assert!(refusal.contains("tilo login"), "{refusal}");

        // A workspace the device is a member of takes the run: its address book lands there as
        // the migration's agent in it, and the device's own workspace has none of it.
        let lab = made(host.as_ref(), "Lab").await;
        // Contacts is installed in the lab as a member installs an app in the workspace they
        // keep things in: an app not installed there holds no grant over its own records, and
        // its records it then lands are refused.
        host.install_app_into(&lab, "so.tilo.contacts").await;
        let into_lab = steps_in(&lab);
        let reviewed = step(
            &into_lab,
            "review",
            json!({"source": "thunderbird"}),
            Asks::Leave,
        )
        .await;
        let book = reviewed.answer["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["kind"] == "contacts")
            .and_then(|item| item["key"].as_str())
            .unwrap_or_else(|| panic!("the review offers the address book: {:#}", reviewed.answer))
            .to_owned();
        step(
            &into_lab,
            "choose",
            json!({"source": "thunderbird", "items": [book], "only": true}),
            Asks::Leave,
        )
        .await;
        let landed = step(
            &into_lab,
            "start",
            json!({"source": "thunderbird"}),
            Asks::Leave,
        )
        .await;
        assert_eq!(landed.answer["state"], "complete", "{:#}", landed.answer);
        assert!(
            listed_in(&agent, &lab, "contacts")
                .await
                .contains("Grace Hopper"),
            "the address book is the lab's"
        );
        assert!(
            !listed_in(&agent, host.workspace(), "contacts")
                .await
                .contains("Grace Hopper"),
            "the device's own workspace has none of it"
        );
        // Migrate keeps its own state in the workspace it is installed in: the lab holds no
        // migration record of its own, and the device's own workspace lists the lab's.
        let in_lab = migrations_offered(host.as_ref(), &lab).await;
        assert!(
            in_lab.is_empty(),
            "the lab holds no Migrate records: {in_lab:?}"
        );
        assert!(
            !migrations_offered(host.as_ref(), host.workspace())
                .await
                .is_empty(),
            "the device's own workspace lists the lab's migration"
        );

        // The workspace the device is in runs by its id: what follows lands there.
        let steps = steps_in(host.workspace());

        let reviewed = step(
            &steps,
            "review",
            json!({"source": "thunderbird"}),
            Asks::Leave,
        )
        .await;
        let account = reviewed.answer["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["kind"] == "account" && item["key"] == WORK_EMAIL)
            .unwrap_or_else(|| panic!("the review offers the work account: {:#}", reviewed.answer));
        assert_eq!(
            account["chosen"], false,
            "nothing is chosen before the person chooses"
        );
        step(
            &steps,
            "choose",
            json!({"source": "thunderbird", "items": [WORK_EMAIL], "only": true}),
            Asks::Leave,
        )
        .await;

        // With nobody to ask, the terminal says no: the account stays unconnected, and the run
        // says so. The question it answered named the migration's agent and the account.
        let declined = step(
            &steps,
            "start",
            json!({"source": "thunderbird"}),
            Asks::Decline,
        )
        .await;
        let asked: Vec<&String> = declined
            .said
            .iter()
            .filter(|line| line.contains("(asked by"))
            .collect();
        assert_eq!(
            asked.len(),
            1,
            "one question, for the one account: {:?}",
            declined.said
        );
        assert!(asked[0].contains(WORK_EMAIL), "{asked:?}");
        assert!(
            declined
                .said
                .iter()
                .any(|line| line.contains("You said no")),
            "{:?}",
            declined.said
        );
        assert!(
            declined
                .said
                .iter()
                .any(|line| line.contains(WORK_EMAIL) && line.contains("left unconnected")),
            "the run says the account is left unconnected: {:?}",
            declined.said
        );
        assert!(!kept_accounts(&agent).await.contains(&WORK_EMAIL.to_owned()));
        assert!(
            steps
                .questions()
                .await
                .expect("the questions read")
                .is_empty(),
            "no question waits once it is answered"
        );

        // `--yes` says yes beforehand: the account is kept in the workspace's mail accounts.
        let approved = step(
            &steps,
            "start",
            json!({"source": "thunderbird"}),
            Asks::Approve,
        )
        .await;
        assert!(
            approved
                .said
                .iter()
                .any(|line| line.contains("You said yes")),
            "{:?}",
            approved.said
        );
        assert!(
            approved
                .said
                .iter()
                .any(|line| line.contains(WORK_EMAIL) && line.contains("is connected")),
            "{:?}",
            approved.said
        );
        assert!(kept_accounts(&agent).await.contains(&WORK_EMAIL.to_owned()));
        let status = step(
            &steps,
            "status",
            json!({"source": "thunderbird"}),
            Asks::Leave,
        )
        .await;
        assert_eq!(status.answer["state"], "complete", "{:#}", status.answer);
        drop(agent);
    });
}

/// The migrations the Migrations app offers in `workspace`, as the person's catalogue lists
/// them there: the resources its `start` is published on.
async fn migrations_offered(host: &host::Host, workspace: &str) -> Vec<String> {
    let page = host
        .dispatch()
        .query(
            &host.caller().in_workspace(workspace.to_owned()),
            CatalogueQuery {
                app_id: Some(MIGRATE.to_owned()),
                action: Some("start".to_owned()),
                ..CatalogueQuery::default()
            },
        )
        .await
        .expect("the catalogue answers")
        .expect("the catalogue lists the app");
    page.rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        // A migration's own record, not the app's surface row for its type: a record is
        // published by its id (`tl:<app>:<id>`), a type by its name (`tl.<app>.<Type>`).
        .filter(|action| action.address.action == "start" && action.address.resource.contains(':'))
        .map(|action| action.address.resource.clone())
        .collect()
}
