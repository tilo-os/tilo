//! A person's approving device, for a stack whose person was made in a page.
//!
//! A browser person's root never leaves the page, and every question a host
//! puts to them — a page action a window holds, say — goes to the devices
//! that hold their root. This program is one of those devices on a machine
//! that runs a local tilo.sh: [`Command::Person`] opens the person a page
//! made from the recovery key it showed, through the product's own root
//! envelope, and keeps their root in an identity root directory;
//! [`Command::Next`] signs their answer to the next question the host has for
//! them, as a member's root does, and prints the question it saw.
//!
//! usage: tilo-approve person --person PERSON.json --root DIR
//!        tilo-approve next --root DIR --relay KEY@HOST:PORT [--answer allow|deny|none] [--within SECS]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tilo_relay_client::questions::{MemberQuestions, QuestionHost};
use tilo_relay_client::{Host, MemberApprover};
use tilo_relay_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_user_identity::IdentityPaths;
use tilo_user_identity::persist::{write_public_key, write_signing_key};
use tilo_user_identity::root_envelope::{RecoveryKey, RootEnvelope};

/// How often a watching device asks the host whether a question is waiting.
const ASK_EVERY: Duration = Duration::from_millis(200);

#[derive(Parser)]
#[command(
    name = "tilo-approve",
    about = "Hold a page-made person, and answer what a host asks them"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open the person a page made into an identity root directory, with the
    /// recovery key that page showed.
    Person {
        /// What the page's person wrote: `{"recovery_key": …, "envelope": {…}}`.
        #[arg(long)]
        person: PathBuf,
        /// The identity root directory the person's root is written into.
        #[arg(long)]
        root: PathBuf,
    },
    /// Answer the next question the host puts to the person.
    Next {
        /// The identity root directory [`Command::Person`] wrote.
        #[arg(long)]
        root: PathBuf,
        /// The host as a page's `TILO_HOSTED_BROWSER_RELAY` names it:
        /// `KEY@HOST:PORT`.
        #[arg(long)]
        relay: String,
        /// `allow` or `deny` the next question, or `none` to answer nothing
        /// and only say what was asked.
        #[arg(long, default_value = "allow")]
        answer: String,
        /// How long a question is waited for, in seconds.
        #[arg(long, default_value_t = 120)]
        within: u64,
    },
}

/// What a page's person writes for this program: the envelope that carries
/// their root, and the recovery key that opens it.
#[derive(Deserialize)]
struct PagePerson {
    recovery_key: String,
    envelope: RootEnvelope,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    Allow,
    Deny,
    /// Answer nothing: say what was asked, so a check can hold a host to
    /// asking nothing at all.
    None,
}

impl Answer {
    fn parse(text: &str) -> anyhow::Result<Self> {
        match text {
            "allow" => Ok(Self::Allow),
            "deny" => Ok(Self::Deny),
            "none" => Ok(Self::None),
            other => bail!("--answer is `allow`, `deny` or `none`, not {other:?}"),
        }
    }
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    match Cli::parse().command {
        Command::Person { person, root } => {
            let public = open_person(&person, &root)?;
            println!(
                "{}",
                serde_json::json!({"person": hex::encode(public), "root": root})
            );
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Next {
            root,
            relay,
            answer,
            within,
        } => {
            let answer = Answer::parse(&answer)?;
            let answered = tokio::runtime::Runtime::new()
                .context("start a runtime")?
                .block_on(next(&root, &relay, answer, Duration::from_secs(within)))?;
            Ok(std::process::ExitCode::from(answered))
        }
    }
}

/// Write the person `document` names into the identity root `root`: the
/// root's own key pair, as every program that holds a person's root keeps
/// it. Answers the person's public key.
fn open_person(document: &std::path::Path, root: &std::path::Path) -> anyhow::Result<[u8; 32]> {
    let page: PagePerson = serde_json::from_slice(
        &std::fs::read(document)
            .with_context(|| format!("read the page's person at {}", document.display()))?,
    )
    .with_context(|| {
        format!(
            "{} is not the JSON a page's person writes",
            document.display()
        )
    })?;
    let recovery = RecoveryKey::parse(&page.recovery_key)
        .map_err(|error| anyhow::anyhow!("the recovery key is not one: {error}"))?;
    let signing = page
        .envelope
        .open_with_recovery(&recovery)
        .map_err(|error| anyhow::anyhow!("the recovery key does not open this person: {error}"))?;
    let public = signing.verifying_key().to_bytes();
    let paths = IdentityPaths::under(root);
    paths
        .ensure_directories()
        .with_context(|| format!("make the identity root at {}", root.display()))?;
    write_signing_key(&paths.user_privkey, &signing)
        .with_context(|| format!("write {}", paths.user_privkey.display()))?;
    write_public_key(&paths.user_pubkey, &public)
        .with_context(|| format!("write {}", paths.user_pubkey.display()))?;
    Ok(public)
}

/// Answer the next question the host at `relay` puts to the person whose
/// root `root` holds: their root signs each command, as a member's does.
/// Answers the exit status: a question answered, no question seen while
/// watching, a question found by a watcher, or none offered in time.
async fn next(
    root: &std::path::Path,
    relay: &str,
    answer: Answer,
    within: Duration,
) -> anyhow::Result<u8> {
    let paths = IdentityPaths::under(root);
    let root_key = tilo_user_identity::persist::read_owned_signing_key(&paths.user_privkey)
        .with_context(|| {
            format!(
                "read the person's root at {}: run `tilo-approve person` first",
                paths.user_privkey.display()
            )
        })?;
    let host = tilo_relay_protocol::hosted::endpoint(relay)
        .map_err(|error| anyhow::anyhow!("--relay: {error}"))?;
    let member = MemberApprover::new(Host::at(host));
    let seen = {
        let mut questions = MemberQuestions {
            host: &member,
            root: &root_key,
        };
        wait(&mut questions, answer, within).await
    };
    member.close().await;
    seen
}

/// Watch the host until a question arrives, or `within` passes.
async fn wait(
    questions: &mut impl QuestionHost,
    answer: Answer,
    within: Duration,
) -> anyhow::Result<u8> {
    let deadline = Instant::now()
        .checked_add(within)
        .unwrap_or_else(Instant::now);
    let mut watched: Vec<serde_json::Value> = Vec::new();
    loop {
        for question in questions
            .confirmations()
            .await
            .map_err(|error| anyhow::anyhow!("ask the host what it has for the person: {error}"))?
        {
            let seen = described(&question);
            println!("{seen}");
            match answer {
                Answer::None => {
                    watched.push(seen);
                }
                Answer::Allow | Answer::Deny => {
                    let decided = match answer {
                        Answer::Allow => ConfirmationAnswer::AllowOnce,
                        _ => ConfirmationAnswer::Deny,
                    };
                    questions
                        .answer(question.id, decided)
                        .await
                        .map_err(|error| anyhow::anyhow!("answer the host's question: {error}"))?;
                    return Ok(0);
                }
            }
        }
        if answer == Answer::None {
            if !watched.is_empty() {
                println!(
                    "{}",
                    serde_json::json!({"watched": watched.len(), "answered": null})
                );
                return Ok(1);
            }
            if Instant::now() >= deadline {
                println!("{}", serde_json::json!({"question": null, "watched": 0}));
                return Ok(0);
            }
        } else if Instant::now() >= deadline {
            println!("{}", serde_json::json!({"question": null}));
            return Ok(3);
        }
        tokio::time::sleep(ASK_EVERY).await;
    }
}

/// One question as this program says it: the action it runs, what it acts
/// on, who asks, where, and what it risks — and, for a page about to send
/// something, the page it leaves from, what it sends there and the picture of
/// the filled page the person is shown.
fn described(question: &PendingConfirmation) -> serde_json::Value {
    serde_json::json!({
        "question": {
            "action": question.action_label,
            "resource": question.resource_label,
            "agent": question.agent_label,
            "run": question.run,
            "workspace": question.workspace,
            "risk": question.risk,
            "kind": question.kind,
            "arguments": question.arguments,
            "page": question.page.as_ref().map(|page| serde_json::json!({
                "page": page.page,
                "kind": page.kind,
                "method": page.method,
                "destination": page.destination,
                "picture": page.picture,
            })),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    /// What a page writes for a person, and what this program opens: the
    /// envelope a page keeps and the recovery key it showed, nothing else.
    #[test]
    fn a_page_person_opens_into_an_identity_root() {
        let directory = tempfile::tempdir().unwrap();
        let page = directory.path().join("person.json");
        let root = directory.path().join("root");
        let signing = SigningKey::from_bytes(&[7; 32]);
        let recovery = RecoveryKey::generate().unwrap();
        let envelope = RootEnvelope::create(&signing, &recovery).unwrap();
        std::fs::write(
            &page,
            serde_json::to_vec(&serde_json::json!({
                "person": hex::encode(signing.verifying_key().to_bytes()),
                "recovery_key": recovery.display().to_string(),
                "envelope": envelope,
            }))
            .unwrap(),
        )
        .unwrap();

        let public = open_person(&page, &root).unwrap();

        assert_eq!(
            public,
            signing.verifying_key().to_bytes(),
            "the person the page made"
        );
        let kept = tilo_user_identity::persist::read_owned_signing_key(
            &IdentityPaths::under(&root).user_privkey,
        )
        .unwrap();
        assert_eq!(
            kept.to_bytes(),
            signing.to_bytes(),
            "the root, kept where every program reads it"
        );
    }

    /// A recovery key of somebody else's does not open a person.
    #[test]
    fn another_persons_recovery_key_opens_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let page = directory.path().join("person.json");
        let signing = SigningKey::from_bytes(&[9; 32]);
        let envelope = RootEnvelope::create(&signing, &RecoveryKey::generate().unwrap()).unwrap();
        std::fs::write(
            &page,
            serde_json::to_vec(&serde_json::json!({
                "recovery_key": RecoveryKey::generate().unwrap().display().to_string(),
                "envelope": envelope,
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(
            open_person(&page, &directory.path().join("root")).is_err(),
            "a wrong key opens no root"
        );
    }

    /// The question a person reads names what it is about.
    #[test]
    fn a_question_says_what_it_is_about() {
        let question = PendingConfirmation {
            action_label: "Submit the visa application".into(),
            resource_label: "browser.page".into(),
            agent_label: "Jane Doe's agent".into(),
            workspace: "ws-browser".into(),
            risk: "outside".into(),
            ..PendingConfirmation::default()
        };

        let said = described(&question);

        assert_eq!(said["question"]["action"], "Submit the visa application");
        assert_eq!(said["question"]["agent"], "Jane Doe's agent");
        assert_eq!(said["question"]["workspace"], "ws-browser");
    }
}
