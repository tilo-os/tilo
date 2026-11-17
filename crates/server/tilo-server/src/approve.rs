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
//! [`Command::Pair`] certifies another device of the person with their root, a
//! device that holds no root (a phone, a second computer); `next --device`
//! answers as that device, as a device paired to the person does.
//!
//! usage: tilo-approve person --person PERSON.json --root DIR
//!        tilo-approve pair --root DIR --device DIR [--label NAME]
//!        tilo-approve next --root DIR --server KEY@HOST:PORT [--device DIR] [--answer allow|deny|none] [--within SECS]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tilo_server_client::questions::{MemberQuestions, QuestionHost};
use tilo_server_client::{Host, MemberApprover};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
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
    /// Certify a new device of the person with their root: a device that
    /// holds no root, as one paired to them.
    Pair {
        /// The identity root directory [`Command::Person`] wrote.
        #[arg(long)]
        root: PathBuf,
        /// Where the device's key and certificate are written.
        #[arg(long)]
        device: PathBuf,
        /// What the device calls itself.
        #[arg(long, default_value = "Phone")]
        label: String,
    },
    /// Answer the next question the host puts to the person.
    Next {
        /// The identity root directory [`Command::Person`] wrote.
        #[arg(long)]
        root: PathBuf,
        /// Answer as the device [`Command::Pair`] wrote here, not the root.
        #[arg(long)]
        device: Option<PathBuf>,
        /// The host as a page's `TILO_HOSTED_BROWSER_SERVER` names it:
        /// `KEY@HOST:PORT`.
        #[arg(long)]
        server: String,
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
        Command::Pair {
            root,
            device,
            label,
        } => {
            let cert = pair(&root, &device, &label)?;
            println!(
                "{}",
                serde_json::json!({"device": hex::encode(cert.device_public_key), "label": label})
            );
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Next {
            root,
            device,
            server,
            answer,
            within,
        } => {
            let answer = Answer::parse(&answer)?;
            let answered = tokio::runtime::Runtime::new()
                .context("start a runtime")?
                .block_on(next(
                    &root,
                    device.as_deref(),
                    &server,
                    answer,
                    Duration::from_secs(within),
                ))?;
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

/// The file a paired device's key is kept in, under its directory.
const DEVICE_KEY: &str = "device.key";
/// The file a paired device's certificate is kept in, under its directory.
const DEVICE_CERT: &str = "device.cert.json";

/// Certify a fresh key as a device of the person whose root `root` holds,
/// with a device's member capabilities and no root, and keep both under
/// `device`.
fn pair(
    root: &std::path::Path,
    device: &std::path::Path,
    label: &str,
) -> anyhow::Result<tilo_user_identity::DeviceCert> {
    let root_key = tilo_user_identity::persist::read_owned_signing_key(
        &IdentityPaths::under(root).user_privkey,
    )
    .context("read the person's root: run `tilo-approve person` first")?;
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|error| anyhow::anyhow!("no randomness: {error}"))?;
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root_key.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root_key)
        .map_err(|error| anyhow::anyhow!("certify the device: {error}"))?;
    std::fs::create_dir_all(device).with_context(|| format!("make {}", device.display()))?;
    write_signing_key(&device.join(DEVICE_KEY), &key)
        .with_context(|| format!("write the device's key under {}", device.display()))?;
    std::fs::write(device.join(DEVICE_CERT), serde_json::to_vec(&cert)?)
        .with_context(|| format!("write the device's certificate under {}", device.display()))?;
    Ok(cert)
}

/// Answer the next question the host at `server` puts to the person whose
/// root `root` holds: their root signs each command, as a member's does, or
/// the device `device` holds when one is named, as a paired device does.
/// Answers the exit status: a question answered, no question seen while
/// watching, a question found by a watcher, or none offered in time.
async fn next(
    root: &std::path::Path,
    device: Option<&std::path::Path>,
    server: &str,
    answer: Answer,
    within: Duration,
) -> anyhow::Result<u8> {
    let host = tilo_server_protocol::hosted::endpoint(server)
        .map_err(|error| anyhow::anyhow!("--server: {error}"))?;
    if let Some(device) = device {
        let key = tilo_user_identity::persist::read_owned_signing_key(&device.join(DEVICE_KEY))
            .context("read the device's key: run `tilo-approve pair` first")?;
        let cert: tilo_user_identity::DeviceCert =
            serde_json::from_slice(&std::fs::read(device.join(DEVICE_CERT))?)
                .context("read the device's certificate")?;
        let member = MemberApprover::new(Host::at(host));
        let seen = {
            let mut questions = MemberQuestions {
                host: &member,
                root: &key,
                device: Some(&cert),
            };
            wait(&mut questions, answer, within).await
        };
        member.close().await;
        return seen;
    }
    let paths = IdentityPaths::under(root);
    let root_key = tilo_user_identity::persist::read_owned_signing_key(&paths.user_privkey)
        .with_context(|| {
            format!(
                "read the person's root at {}: run `tilo-approve person` first",
                paths.user_privkey.display()
            )
        })?;
    let member = MemberApprover::new(Host::at(host));
    let seen = {
        let mut questions = MemberQuestions {
            host: &member,
            root: &root_key,
            device: None,
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
            "title": question.title,
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
    use tilo_server_protocol::certify::PageDeparture;

    tilo_test_env::scenario! {
        fn a_page_person_opens_only_with_its_own_recovery_key;

        phase a_page_person_opens_into_an_identity_root;
        // What a page writes for a person, and what this program opens: the
        // envelope a page keeps and the recovery key it showed, nothing else.
        {
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

        phase another_persons_recovery_key_opens_nothing;
        // A recovery key of somebody else's does not open a person.
        {
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
    }

    /// The question a person reads names what it is about, and a page it
    /// leaves from by its origin, what it sends there and the picture of the
    /// filled page.
    #[test]
    fn a_question_says_what_it_is_about() {
        let question = PendingConfirmation {
            action_label: "Submit the visa application".into(),
            resource_label: "browser.page".into(),
            agent_label: "Jane Doe's agent".into(),
            workspace: "ws-browser".into(),
            risk: "outside".into(),
            page: Some(PageDeparture {
                page: "https://consulate.example".into(),
                kind: "submit".into(),
                method: "POST".into(),
                destination: "https://consulate.example".into(),
                picture: Some("tilo.content:5f2b".into()),
            }),
            ..PendingConfirmation::default()
        };

        let said = described(&question);

        assert_eq!(said["question"]["action"], "Submit the visa application");
        assert_eq!(said["question"]["agent"], "Jane Doe's agent");
        assert_eq!(said["question"]["workspace"], "ws-browser");
        assert_eq!(
            said["question"]["page"]["page"],
            "https://consulate.example"
        );
        assert_eq!(said["question"]["page"]["method"], "POST");
        assert_eq!(said["question"]["page"]["kind"], "submit");
        assert_eq!(said["question"]["page"]["picture"], "tilo.content:5f2b");
        // A question about no page says nothing about one.
        let plain = PendingConfirmation {
            page: None,
            ..question
        };
        assert_eq!(
            described(&plain)["question"]["page"],
            serde_json::Value::Null
        );
    }
}
