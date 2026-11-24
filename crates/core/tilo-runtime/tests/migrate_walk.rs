//! `tilo migrate` as a person runs it, through the device's daemon: the steps are the ones the
//! command takes ([`tilo_migrate::provider::caller::Steps`]), each an invocation of the device's
//! migrations followed to its end, and the question a run asks its person (keeping a Thunderbird
//! mail account) is the asked record the workspace lists, answered with its own `resolve`, as the
//! terminal answers it. `-w` names the workspace the device is in; another is refused naming the
//! command that joins it.
//!
//! Nothing stands in but the mail server's sign-in: the daemon serves its own migrations over a
//! Thunderbird profile in the test's home, the run keeps the account through the workspace's
//! `workspace/mail-accounts#add` as the device's migration agent, and the workspace's policy
//! asks the person first.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_actions::remote::asked::Asked;
use tilo_migrate::provider::caller::{Answering, Asks, Steps, question_said};
use tilo_migrate::thunderbird::fixture::WORK_EMAIL;

use crate::host;
use crate::migrate_window::{agent_does, engine};

/// What a step answered, and every line it said on the way, the questions asked included.
struct Stepped {
    answer: Value,
    said: Vec<String>,
}

/// The step `action` of the device's migrations with `args`, as `tilo migrate` takes it,
/// answering the questions its run asks as `asks` says.
async fn step(steps: &Arc<Steps>, action: &str, args: Value, asks: Asks) -> Stepped {
    let (steps, action) = (Arc::clone(steps), action.to_owned());
    tokio::task::spawn_blocking(move || {
        let Value::Object(args) = args else {
            panic!("a step's arguments are an object")
        };
        let said = Arc::new(Mutex::new(Vec::new()));
        let telling = Arc::clone(&said);
        let mut say = move |line: &str| telling.lock().unwrap().push(line.to_owned());
        let mut prompt = |question: &Asked| -> anyhow::Result<bool> {
            panic!("nobody is at a terminal here: {}", question_said(question))
        };
        let answer = steps
            .step(
                &action,
                args,
                &mut Answering {
                    asks,
                    prompt: &mut prompt,
                    say: &mut say,
                },
            )
            .unwrap_or_else(|error| panic!("{action} reaches the device: {error:#}"))
            .unwrap_or_else(|failure| panic!("{action} runs: {failure:?}"));
        let said = said.lock().unwrap().clone();
        Stepped { answer, said }
    })
    .await
    .unwrap()
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
/// `-w` naming a workspace the device is not in is refused with the command that joins it; the
/// workspace it is in runs, and the run lands there.
#[test]
fn the_terminal_walks_a_thunderbird_migration_on_the_device() {
    host::run_with(&["migrate"], |host| async move {
        let home = PathBuf::from(std::env::var_os("HOME").expect("the test's home"));
        tilo_migrate::thunderbird::fixture::write_sample_profile(&home);
        // The mail server is the one thing stood in for: it takes the password Thunderbird saved.
        host.daemon()
            .workspaces()
            .sign_in_accounts_through(|_, password| {
                if password == tilo_migrate::thunderbird::fixture::WORK_PASSWORD {
                    Ok("signed in".into())
                } else {
                    Err("IMAP server rejected the credential".into())
                }
            });
        host.daemon().serve_migrations();
        let device = device_of(&engine(host.as_ref()).await);
        let steps = Arc::new(Steps::new(
            host.dispatch(),
            host.caller(),
            &device,
            host.workspace().to_owned(),
            None,
        ));
        let agent = host.agent();

        // `-w` names a workspace: one the device is not in is refused, naming how to join it.
        let elsewhere = "00000000-0000-4000-8000-000000000000";
        let refused = steps
            .in_workspace(elsewhere)
            .expect_err("the device is in no such workspace");
        let refusal = format!("{refused:#}");
        assert!(refusal.contains("tilo login"), "{refusal}");
        assert!(refusal.contains(elsewhere), "{refusal}");
        // The workspace the device is in runs: what follows lands there.
        steps
            .in_workspace(host.workspace())
            .expect("the device is in its own workspace");

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
            steps.questions().expect("the questions read").is_empty(),
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
