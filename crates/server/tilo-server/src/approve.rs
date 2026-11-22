//! A person's approving device, for a stack whose person was made in a page.
//!
//! A browser person's root never leaves the page, and every question a host
//! puts to them — a page action a window holds, say — goes to the devices
//! that hold their root. This program is one of those devices on a machine
//! that runs a local tilo.sh: [`Command::Person`] opens the person a page
//! made from the recovery key it showed, through the product's own root
//! envelope, and keeps their root in an identity store of its own, as a
//! server keeps a root: in owner-only files, since it has no Secret Service;
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
use tilo_device_identity::{IdentityPaths, persist};
use tilo_enrollment::root::PersonSigner;
use tilo_server_client::questions::{MemberQuestions, QuestionServer};
use tilo_server_client::{MemberApprover, ServerAddress};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
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
        /// The identity directory the person's root is kept in.
        #[arg(long)]
        root: PathBuf,
    },
    /// Certify a new device of the person with their root: a device that
    /// holds no root, as one paired to them.
    Pair {
        /// The identity directory [`Command::Person`] kept the root in.
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
        /// The identity directory [`Command::Person`] kept the root in.
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

/// Keep the person `document` names in the identity store at `root`: their
/// whole root as a single-device identity, its secret in owner-only files.
/// Answers the person's public key.
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
    tilo_identity::provisioning::import_single_device_root(
        root,
        &signing,
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .with_context(|| format!("keep the person's root in {}", root.display()))?;
    Ok(signing.verifying_key().to_bytes())
}

/// The person's signer for the host's questions.
fn person_root(root: &std::path::Path) -> anyhow::Result<PersonSigner> {
    let active = tilo_identity_client::resolve_active_identity(root).with_context(|| {
        format!(
            "{} holds no person: run `tilo-approve person` first",
            root.display()
        )
    })?;
    let identity = tilo_identity::store::load_from_path(active)
        .map_err(|error| anyhow::anyhow!("open the person's root: {error}"))?;
    Ok(PersonSigner::new(identity))
}

/// Certify a fresh key as a device of the person whose root `root` holds,
/// with a device's member capabilities and no root, and keep both under
/// `device`.
fn pair(
    root: &std::path::Path,
    device: &std::path::Path,
    label: &str,
) -> anyhow::Result<tilo_user_identity::DeviceCert> {
    let root_key = tilo_enrollment::load_root(Some(root))?;
    let paths = IdentityPaths::under(device);
    let private = paths
        .device_privkey
        .parent()
        .context("the device key has no directory")?;
    tilo_files::native::private::directory(private)?;
    persist::provision_joined_device(
        &paths,
        root_key.public_key(),
        |digest| {
            let digest = digest
                .try_into()
                .map_err(|_| "the certificate digest must be 32 bytes".to_owned())?;
            root_key
                .sign_digest(digest)
                .map_err(|error| error.to_string())
        },
        label,
        tilo_clock::unix_now_ns(),
    )
    .with_context(|| format!("enroll the device under {}", device.display()))
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
        let paths = IdentityPaths::under(device);
        let key = persist::load_device_key(&paths)
            .context("read the device's key: run `tilo-approve pair` first")?;
        let cert = persist::read_device_cert(&paths.device_cert)
            .context("read the device's certificate")?;
        let member = MemberApprover::new(ServerAddress::at(host));
        let seen = {
            let mut questions = MemberQuestions {
                server: &member,
                root: &key,
                device: Some(&cert),
            };
            wait(&mut questions, answer, within).await
        };
        member.close().await;
        return seen;
    }
    let root_key = person_root(root)?;
    let member = MemberApprover::new(ServerAddress::at(host));
    let seen = {
        let mut questions = MemberQuestions {
            server: &member,
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
    questions: &mut impl QuestionServer,
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
            let kept = tilo_enrollment::load_root(Some(&root)).unwrap();
            assert_eq!(
                kept.public_key(),
                signing.verifying_key().to_bytes(),
                "the root, kept in the person's identity store"
            );
            assert_eq!(
                tilo_identity::store::list_identities(&root).unwrap()[0].protection,
                tilo_identity_client::KeyProtection::BrokerFile,
                "in owner-only files, as a server keeps it"
            );

            // A device paired with that root: certified by it, holding no root.
            let phone = directory.path().join("phone");
            let cert = pair(&root, &phone, "Phone").unwrap();
            tilo_user_identity::verify_device_cert(&cert).unwrap();
            assert_eq!(cert.user_public_key, public);
            assert_eq!(cert.device_label, "Phone");
            assert!(!cert.capabilities.can_admit_devices);
            let paths = IdentityPaths::under(&phone);
            assert_eq!(persist::read_device_cert(&paths.device_cert).unwrap(), cert);
            assert_eq!(
                persist::load_device_key(&paths).unwrap().verifying_key().to_bytes(),
                cert.device_public_key
            );
            assert!(pair(&root, &phone, "Phone").is_err());
            assert_eq!(persist::read_device_cert(&paths.device_cert).unwrap(), cert);
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
