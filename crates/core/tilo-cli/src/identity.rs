//! A person whose root this computer holds (`tilo login --new-identity`),
//! and what that root lets this computer do without a browser: approve the
//! key `tilo do account/keys#create` asks for, as the person's Tilo on the web does.
//!
//! The person is this account's one Tilo identity, kept where its session
//! keeps it: their identity in `$XDG_CONFIG_HOME/tilo/identity` with its secret
//! in the platform's secret store (or its private-file fallback), and this
//! computer's device key and the certificate the root signed for it under the
//! device's identity root
//! ([`collaboration_identity_root`](tilo_identity::provisioning::collaboration_identity_root)).
//! A Tilo session started on this account is that same person.
//! Approving a key asks the site only to take the person's proof and the key's
//! certificate: nothing is read from the site's own entry, and no site's host
//! is dialled.

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use tilo_device_identity::IdentityPaths;
use tilo_device_identity::persist::read_device_cert;
use tilo_identity::provisioning::IdentityStorage;
use tilo_user_identity::DeviceCert;

use crate::login;

/// The person this computer holds the root of.
///
/// Their root and this computer's device key stay where they are kept: the
/// daemon that keeps them signs for both ([`tilo_device_identity::login::ANSWER_SIGN_IN`],
/// [`tilo_device_identity::login::SIGN`]).
pub struct Person {
    /// The certificate the root signed for this computer's device key.
    pub cert: DeviceCert,
    /// How the person is named: empty while they gave no name.
    pub name: String,
}

impl Person {
    /// Who the person is in a sentence said to them: their name, or "you" while they gave none.
    #[must_use]
    pub fn who(&self) -> &str {
        if self.name.is_empty() {
            "you"
        } else {
            &self.name
        }
    }

    /// Whose a thing of theirs is, in a sentence said to them: "Ada's", or "your".
    #[must_use]
    pub fn whose(&self) -> String {
        if self.name.is_empty() {
            "your".to_owned()
        } else {
            format!("{}'s", self.name)
        }
    }

    /// Their user public key, hex.
    pub fn user(&self) -> String {
        hex::encode(self.public_key())
    }

    /// Their user public key.
    pub fn public_key(&self) -> [u8; 32] {
        self.cert.user_public_key
    }
}

/// This account's identity directory, the session's own.
pub fn dir() -> Result<PathBuf> {
    tilo_identity::store::identity_dir_from_env().context("this account has no identity directory")
}

/// This computer's device key and certificates, the session's own.
pub fn paths() -> IdentityPaths {
    tilo_identity::provisioning::collaboration_identity_paths()
}

/// The active identity's file, when this computer holds its root.
fn root_identity() -> Option<PathBuf> {
    // The person is the account's own, in the store it reads: no identity is kept beside it.
    tilo_identity::store::held_root(&tilo_identity::store::account_identity_dir(&dir().ok()?))
}

/// Whether this computer keeps a person's root, without reading it: a process
/// that reads it makes itself undumpable ([`held`]).
pub fn holds_root() -> bool {
    root_identity().is_some()
}

/// The person kept here, if this computer holds their root.
pub fn held() -> Result<Option<Person>> {
    let Some(active) = root_identity() else {
        return Ok(None);
    };
    // What the record says of the person, without their root: this process never reads it.
    let metadata = tilo_identity::store::load_metadata_from_path(&active)
        .map_err(|error| anyhow!("{} cannot be opened: {error}", active.display()))?;
    let name = metadata.display_name.trim();
    let name = if tilo_identity::store::is_chosen_display_name(name) {
        name.to_owned()
    } else {
        String::new()
    };
    let user = metadata.group_public_key;
    let paths = paths();
    let cert = read_device_cert(&paths.device_cert_for_user(&user))
        .or_else(|_| read_device_cert(&paths.device_cert))
        .ok()
        .filter(|cert| cert.user_public_key == user)
        .with_context(|| {
            format!(
                "this computer holds no certificate of {} for its device: start Tilo here once                  so it certifies this device",
                hex::encode(user)
            )
        })?;
    Ok(Some(Person { cert, name }))
}

/// A ticket the founding call issued as part of making the person, which their own act asks
/// for: what it lets in, and the ticket itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirstTicket {
    /// `server` for a server, `device` for a computer at hand.
    pub kind: String,
    pub ticket: String,
}

/// The first tickets a founding asked for: those it was answered, or why it was not.
#[derive(Clone, Debug, Default)]
pub struct FirstTickets {
    pub tickets: Vec<FirstTicket>,
    /// Why the tickets asked for were not issued, when they were not.
    pub error: Option<String>,
}

/// The person kept here, made first when there is none: whether it was made now, and the tickets
/// `tickets` asks the founding for. `name` names a new one; one already kept stays as it is.
///
/// A person is made one way: this computer's daemon makes them (`devices#found`), with the class
/// this computer is, so it needs the daemon running; nothing makes one here instead. The tickets
/// come back from that same call, as the person's own first act: waiting for them another way is
/// asking the person to approve, which no one at a fresh computer can.
pub fn ensure(
    name: Option<&str>,
    tickets: &[tilo_api::daemon::TicketKind],
) -> Result<(Person, bool, FirstTickets)> {
    let made = !holds_root();
    // A person goes by the name they give, else by none until they give one: never their
    // computer's account name or a made-up one.
    let name = name.map(str::trim).filter(|name| !name.is_empty());
    let mut first = FirstTickets::default();
    if made {
        let founding = tilo_api::daemon::Founding {
            name: name.map(str::to_owned),
            tickets: tickets.to_vec(),
        };
        let founded = login::ensure_daemon_founding(&founding).map(|(_, answer)| answer);
        // The daemon makes this account's person as its first command reaches it, and answers
        // the tickets the founding asked for; a daemon that serves no person yet (the system
        // unit's) is asked outright. A person already kept here is served, and asking again
        // would wait on the person's part the daemon is starting.
        // Why the tickets were not answered, in the daemon's words, when its call failed: a
        // founding that went wrong says what, never only that the tickets are missing.
        let answer = match founded {
            Ok(answer) => {
                if answer.is_some() || held().is_ok_and(|person| person.is_some()) {
                    Ok(answer)
                } else {
                    tilo_api::daemon::this_computer().and_then(|place| place.found(&founding))
                }
            }
            Err(why) if held()?.is_none() => return Err(anyhow::Error::msg(why)),
            Err(why) => Err(format!("{why:#}")),
        };
        first = first_tickets(tickets, answer);
    } else if !tickets.is_empty() {
        // A person made already issues nothing: only the call that made them could ask for
        // their first tickets.
        first.error = Some(
            "this computer already has a person, and only the call that makes one issues its \
             first tickets"
                .to_owned(),
        );
    }
    let mut person =
        held()?.context("this computer's daemon made no person it holds the root of")?;
    // The daemon may have made the person as it started, before it was told their name.
    if made && let Some(name) = name {
        keep_name(&mut person, name)?;
    }
    Ok((person, made, first))
}

/// The tickets `asked` for, read from the founding `answer`: those issued, or why none were.
fn first_tickets(
    asked: &[tilo_api::daemon::TicketKind],
    answer: Result<Option<serde_json::Value>, String>,
) -> FirstTickets {
    if asked.is_empty() {
        return FirstTickets::default();
    }
    let answer = match answer {
        Ok(answer) => answer,
        Err(why) => {
            return FirstTickets {
                tickets: Vec::new(),
                error: Some(why),
            };
        }
    };
    let Some(answer) = answer else {
        return FirstTickets {
            tickets: Vec::new(),
            error: Some("this computer's person was made without the call that issues them".into()),
        };
    };
    let error = answer
        .get("tickets_error")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let tickets = answer
        .get("tickets")
        .and_then(serde_json::Value::as_array)
        .map(|tickets| {
            tickets
                .iter()
                .filter_map(|ticket| {
                    Some(FirstTicket {
                        kind: ticket.get("kind")?.as_str()?.to_owned(),
                        ticket: ticket.get("ticket")?.as_str()?.to_owned(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    FirstTickets { tickets, error }
}

/// Become again the person whose 24-word recovery key `words` is: their root
/// kept here as `tilo login --new-identity` keeps a new one, and this computer
/// certified by it as a device of its own. A computer that holds them already
/// keeps them; one that holds someone else refuses.
///
/// # Errors
/// The words are not a recovery key, they are someone else's than the person
/// kept here, or the identity could not be written or certified.
pub fn restore(words: &str) -> Result<Person> {
    let dir = tilo_identity::store::account_identity_dir(&dir()?);
    tilo_identity::provisioning::restore_from_recovery_words(
        &dir,
        &paths(),
        words,
        Some(&login::label("tilo", &login::hostname())),
        IdentityStorage::for_session(),
    )
    .map_err(
        |error| match tilo_identity::store::recovery_words_error(&error) {
            Some(tilo_identity::store::RecoveryWordsError::DifferentIdentity {
                expected, ..
            }) => {
                anyhow!(
                    "This computer holds another Tilo identity ({}), not the one these words are: \
                 log in as that person, or use a computer account of its own for this one.",
                    hex::encode(expected)
                )
            }
            Some(problem) => anyhow!("{problem}"),
            None => anyhow!(error).context(format!("restore the identity in {}", dir.display())),
        },
    )?;
    held()?.context("the identity just restored is not there")
}

/// Name the person kept here `name`, when they go by none yet: how a person who joins a
/// workspace from a computer that holds their identity already names themselves there.
///
/// # Errors
/// The name could not be written.
pub fn keep_name(person: &mut Person, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || !person.name.is_empty() {
        return Ok(());
    }
    tilo_identity::store::rename_active_identity(&dir()?, name)?;
    name.clone_into(&mut person.name);
    Ok(())
}

/// `tilo login --new-identity`: make the person, or say there is one, and
/// print their user public key — or, as one line of JSON, the key, the
/// name they go by and the first tickets `tickets` asked for, for a script
/// that declares them somewhere and adds the first computers with them.
///
/// # Errors
/// The person could not be made, or the tickets the founding asked for were not issued.
pub fn new_identity(
    name: Option<&str>,
    json: bool,
    tickets: &[tilo_api::daemon::TicketKind],
) -> Result<std::process::ExitCode> {
    let (person, made, first) = ensure(name, tickets)?;
    let dir = dir()?;
    let missed = first.error.clone();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "user_public_key": person.user(),
                "name": person.name,
                "tickets": first
                    .tickets
                    .iter()
                    .map(|ticket| serde_json::json!({"kind": ticket.kind, "ticket": ticket.ticket}))
                    .collect::<Vec<_>>(),
            })
        );
        if let Some(why) = missed {
            return Err(anyhow!(
                "This computer's person is made, but the tickets asked for were not issued: {why}"
            ));
        }
        return Ok(std::process::ExitCode::SUCCESS);
    }
    if made {
        eprintln!(
            "Made {} Tilo identity on this computer ({}). Once a host's operator declares \
             this key an owner, `tilo do account/keys#create` makes its keys here.",
            person.whose(),
            dir.display()
        );
    } else {
        eprintln!(
            "This computer already holds {} Tilo identity ({}).",
            person.whose(),
            dir.display()
        );
    }
    println!("{}", person.user());
    for ticket in &first.tickets {
        let what = if ticket.kind == "server" {
            "a server"
        } else {
            "a computer"
        };
        eprintln!("The ticket for {what}:");
        println!("{}", ticket.ticket);
    }
    if let Some(why) = missed {
        return Err(anyhow!(
            "This computer's person is made, but the tickets asked for were not issued: {why}"
        ));
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// What the person's Tilo is asked to sign at `<site>/sign-in?…`.
#[derive(Debug, PartialEq, Eq)]
pub struct Asked {
    pub audience: String,
    pub challenge: [u8; 32],
    pub return_to: String,
    pub pending: String,
    pub client: String,
    pub grant_key: [u8; 32],
    pub server_key: bool,
    /// What the key reaches: `all` of the person's workspaces, or one workspace's id.
    pub scope: String,
    /// Whether the key operates web pages in Browser without asking each time.
    pub pages: bool,
    pub expires_at_ns: Option<u64>,
}

/// Read the sign-in `location` an approval link leads to.
pub fn asked(location: &str) -> Result<Asked> {
    let url = tilo_net::Url::parse(location)
        .with_context(|| format!("{location} is not a sign-in address"))?;
    let query: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    let field = |name: &str| {
        query
            .get(name)
            .cloned()
            .with_context(|| format!("the sign-in asks for no {name}"))
    };
    let key = |name: &str| -> Result<[u8; 32]> {
        hex::decode(field(name)?)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .with_context(|| format!("the sign-in's {name} is not 32 bytes of hex"))
    };
    let expires_at_ns = match field("grant_expires")?.as_str() {
        "never" => None,
        ms => Some(
            ms.parse::<u64>()
                .context("the sign-in's grant_expires is no time")?
                .saturating_mul(1_000_000),
        ),
    };
    Ok(Asked {
        audience: field("audience")?,
        challenge: key("challenge")?,
        return_to: field("return")?,
        pending: field("pending")?,
        client: query.get("client").cloned().unwrap_or_default(),
        grant_key: key("grant_key")?,
        server_key: query.get("grant_kind").map(String::as_str) == Some("server_key"),
        scope: query
            .get("grant_scope")
            .cloned()
            .unwrap_or_else(|| "all".to_owned()),
        pages: query.get("grant_pages").map(String::as_str) == Some("1"),
        expires_at_ns,
    })
}

/// The proof and the grant certificate `person` answers `asked` with, as
/// the page posts them back: `pending`, `proof` and `grant`, signed by the one
/// sign-in signer the person's Tilo on the web signs with
/// ([`answer_sign_in`](tilo_server_client::certification::answer_sign_in)).
///
/// # Errors
/// A scope that is neither `all` nor a workspace id, a key for one workspace
/// that names no expiry, or a certificate that does not sign.
pub async fn answer(person: &Person, asked: &Asked) -> Result<Vec<(&'static str, String)>> {
    use tilo_user_identity::AgentScope;
    // What the key asks to reach, named the way the daemon that signs reads it.
    AgentScope::parse(&asked.scope).with_context(|| {
        format!(
            "the key asks to reach {}, which is no workspace",
            asked.scope
        )
    })?;
    let asked_args = serde_json::json!({
        "audience": asked.audience,
        "challenge": hex::encode(asked.challenge),
        "client": asked.client,
        "grant": {
            "key": hex::encode(asked.grant_key),
            "scope": asked.scope,
            "expires_at_ns": asked.expires_at_ns,
            "server_key": asked.server_key,
            "browser_pages": asked.pages,
        },
    });
    let pending = asked.pending.clone();
    let answer = tokio::task::spawn_blocking(move || {
        tilo_api::daemon::this_computer()?
            .act(tilo_device_identity::login::ANSWER_SIGN_IN, asked_args)
    })
    .await
    .map_err(|error| anyhow!("the sign-in answer was not awaited: {error}"))?
    .map_err(anyhow::Error::msg)?;
    let proof = answer
        .get("proof")
        .and_then(serde_json::Value::as_str)
        .context("this computer's daemon signed no proof")?
        .to_owned();
    let grant = answer
        .get("grant")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if let Some(grant) = &grant {
        let grant: DeviceCert =
            tilo_user_identity::canonical::from_canonical_bytes(&base64_url(grant)?)
                .map_err(|error| anyhow!("{error}"))?;
        if grant.user_public_key != person.public_key() {
            bail!("the key was certified by another person than the one kept here");
        }
    }
    Ok(vec![
        ("pending", pending),
        ("proof", proof),
        ("grant", grant.unwrap_or_default()),
    ])
}

/// `text`, the URL-safe canonical bytes the site's page reads, as the bytes.
fn base64_url(text: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .context("the answer is not URL-safe base64")
}

/// Approve the key `approve_url` asks for, as `person`'s Tilo on the web
/// does once they agree: the link leads to the site's sign-in, which this
/// answers with the person's proof and the key's certificate.
pub async fn approve(person: &Person, approve_url: &str) -> Result<()> {
    let http = tilo_net::Client::builder()
        .redirects(tilo_net::Redirects::None)
        .build()?;
    let led = http
        .get(approve_url)
        .send()
        .await
        .with_context(|| format!("reaching {approve_url}"))?;
    let location = led
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .with_context(|| format!("{approve_url} leads to no sign-in (HTTP {})", led.status()))?
        .to_owned();
    let asked = asked(&location)?;
    let form = answer(person, &asked).await?;
    let answered = http
        .post(&asked.return_to)
        .form(&form)
        .send()
        .await
        .with_context(|| format!("reaching {}", asked.return_to))?;
    let status = answered.status();
    if !(status.is_success() || status.is_redirection()) {
        bail!("{} refused the approval (HTTP {status})", asked.return_to);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A person made on this computer account as its daemon makes one
    /// ([`tilo_identity::provisioning::create_person`]), for a test that needs one there. The
    /// person is the account's own, in the store it reads.
    fn made_here(name: &str) -> Person {
        tilo_identity::provisioning::create_person(
            &tilo_identity::store::account_identity_dir(&dir().unwrap()),
            &paths(),
            name,
            "",
            IdentityStorage::for_session(),
        )
        .unwrap();
        held().unwrap().expect("the person just made")
    }

    /// The record this account acts as, in the store it reads.
    fn active_here() -> std::path::PathBuf {
        let account = tilo_identity::store::account_identity_dir_from_env().unwrap();
        tilo_identity::store::resolve_active_identity(&account).unwrap()
    }

    /// This computer's device key taken in this process, as its device service holds it while
    /// it runs: the account's root is sealed with that key, so only the process holding it opens
    /// the root. A test has no device service of its own to ask, so it takes the key here.
    fn took_the_device_key() -> tilo_device_identity::persist::DeviceKeyLock {
        let paths = paths();
        tilo_device_identity::persist::load_or_create_device_key(&paths).unwrap();
        tilo_device_identity::persist::take_device_key(&paths).unwrap()
    }

    #[test]
    fn a_person_restored_from_their_recovery_words_is_who_they_were() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let at = |env: &mut tilo_test_env::ScopedEnv, home: &std::path::Path| {
            env.set("HOME", home)
                .set("XDG_CONFIG_HOME", home.join("config"))
                .set("TILO_IDENTITY_ROOT", home.join("system"))
                .set(tilo_identity::store::FILE_SECRETS_ENV, "1");
        };
        let first = tempfile::tempdir().unwrap();
        at(&mut env, first.path());
        let _first_taken = took_the_device_key();
        let ada = made_here("Ada");
        let words = tilo_identity::store::recovery_words(active_here()).unwrap();
        let other = tempfile::tempdir().unwrap();
        at(&mut env, other.path());
        let _other_taken = took_the_device_key();
        made_here("Bea");
        let bea_words = tilo_identity::store::recovery_words(active_here()).unwrap();

        // A computer account with nothing of Tilo on it: the words make it Ada's again, with a
        // device of its own that her root certified.
        let fresh = tempfile::tempdir().unwrap();
        at(&mut env, fresh.path());
        let _fresh_taken = took_the_device_key();
        assert!(!holds_root());
        let restored = restore(&format!("  {}\n", words.as_str())).unwrap();
        assert_eq!(restored.public_key(), ada.public_key(), "the same person");
        assert_eq!(restored.cert.user_public_key, ada.public_key());
        tilo_user_identity::verify_device_cert(&restored.cert).unwrap();
        assert_ne!(
            restored.cert.device_public_key, ada.cert.device_public_key,
            "this computer is a device of its own"
        );
        let profile = tilo_identity::store::identity_for_workspace("any")
            .unwrap()
            .expect("the session's registry names her");
        assert_eq!(profile.user_public_key, ada.public_key());

        // Her words again keep her, on the same device; someone else's are refused.
        let again = restore(&words).unwrap();
        assert_eq!(again.public_key(), ada.public_key());
        assert_eq!(
            again.cert.device_public_key,
            restored.cert.device_public_key
        );
        let refused = restore(&bea_words).err().expect("another person's words");
        assert!(
            refused.to_string().contains("another Tilo identity"),
            "{refused:#}"
        );
        assert_eq!(held().unwrap().unwrap().public_key(), ada.public_key());
        let wrong = restore("abandon ability able").err().expect("three words");
        assert!(wrong.to_string().contains("24 words"), "{wrong:#}");
    }

    #[test]
    fn a_sign_in_link_is_read_as_the_ask_it_is() {
        let location = format!(
            "http://tilo.localhost:24120/sign-in?audience=http%3A%2F%2Flocalhost%3A24110\
             &challenge={}&return=http%3A%2F%2Flocalhost%3A24110%2Fsign-in&pending=p1\
             &client=Operator&grant_key={}&grant_scope=all&grant_expires=never\
             &grant_kind=server_key",
            "07".repeat(32),
            "09".repeat(32),
        );
        let read = asked(&location).unwrap();
        assert_eq!(read.audience, "http://localhost:24110");
        assert_eq!(read.return_to, "http://localhost:24110/sign-in");
        assert!(read.server_key);
        assert_eq!(read.expires_at_ns, None);
        assert_eq!(read.grant_key, [9; 32]);

        // A key for one workspace until a time, which is narrower than a server key.
        let workspace = "3ee4a797-1ec0-474b-a3d0-62895d5e15af";
        let until_ms = tilo_clock::unix_now_ns() / 1_000_000 + 3_600_000;
        let one = asked(
            &location
                .replace("&grant_kind=server_key", "&grant_pages=1")
                .replace("grant_scope=all", &format!("grant_scope={workspace}"))
                .replace("grant_expires=never", &format!("grant_expires={until_ms}")),
        )
        .unwrap();
        assert!(!one.server_key);
        assert!(one.pages);
        assert_eq!(one.scope, workspace);
        assert_eq!(one.expires_at_ns, Some(until_ms * 1_000_000));
    }
}
