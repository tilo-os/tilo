//! A person's connections as their server keeps them: one file each, under
//! the person's own root, beside their vault.
//!
//! A kept connection holds nothing secret. What a service issued at a
//! sign-in is an item of the person's vault, and the connection keeps only
//! which item.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_lock::MutexRecover as _;
use tilo_workspace_records::connections::{Command, Connection, Ended, Leave, SignIn, State, Use};

use super::said::Offered;
use super::sign_in::{Client, Tokens, Way};

/// The most connections one person keeps.
pub(crate) const MAX_CONNECTIONS: usize = 64;

/// The most calls a connection remembers: the newest of them.
pub(crate) const MAX_USES: usize = 200;

/// The folder of a person's root their connections are kept in.
const DIR: &str = "connections";

/// One writer at a time changes what is kept, so a connection removed while
/// its commands were being read stays removed.
static WRITES: Mutex<()> = Mutex::new(());

/// Why a connection is not ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Why {
    NoAnswer,
    SentOn,
    NotCalled,
    TurnedAway,
    Unreadable,
    WantsSignIn,
    SignInRanOut,
}

impl Why {
    /// The system's words for it.
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::NoAnswer => "The service did not answer",
            Self::SentOn => {
                "The service sent the request on to another address, and a server follows none"
            }
            Self::NotCalled => "That address is not called from here",
            Self::TurnedAway => "The service turned the request away",
            Self::Unreadable => "The service's answer could not be read",
            Self::WantsSignIn => "The service wants you to sign in",
            Self::SignInRanOut => "The service no longer takes this sign-in",
        }
    }

    /// How a connection stands for this reason.
    pub(crate) const fn state(self) -> State {
        match self {
            Self::NoAnswer | Self::Unreadable => State::Unreachable,
            Self::SentOn | Self::NotCalled | Self::TurnedAway => State::Refused,
            Self::WantsSignIn => State::NeedsSignIn,
            Self::SignInRanOut => State::SignInRanOut,
        }
    }
}

/// One command of a kept connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct KeptCommand {
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) description: String,
    pub(crate) read_only: bool,
    pub(crate) schema: Value,
    /// What names its definition as the service last listed it.
    pub(crate) digest: String,
    /// What named its definition when its person last saw it; none for a
    /// command they never saw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) seen: Option<String>,
}

impl KeptCommand {
    /// Whether its definition is not the one its person last saw.
    pub(crate) fn changed(&self) -> bool {
        self.seen.as_deref() != Some(self.digest.as_str())
    }

    /// Whether an agent asks before it calls it: every command but one marked
    /// as only reading and defined as its person last saw it.
    pub(crate) fn asks(&self) -> bool {
        !self.read_only || self.changed()
    }

    pub(crate) fn view(&self) -> Command {
        Command {
            name: self.name.clone(),
            title: self.title.clone(),
            description: self.description.clone(),
            read_only: self.read_only,
            asks: self.asks(),
            changed: self.changed(),
            schema: self.schema.clone(),
        }
    }
}

/// The commands `offered` as kept beside `before`, the ones kept until now:
/// a command keeps what its person last saw of it, and with `seen` (the
/// person themselves is reading) every definition is the one they saw.
pub(crate) fn commands(
    offered: Vec<Offered>,
    before: &[KeptCommand],
    seen: bool,
) -> Vec<KeptCommand> {
    offered
        .into_iter()
        .map(|command| {
            let seen = if seen {
                Some(command.digest.clone())
            } else {
                before
                    .iter()
                    .find(|kept| kept.name == command.name)
                    .and_then(|kept| kept.seen.clone())
            };
            KeptCommand {
                name: command.name,
                title: command.title,
                description: command.description,
                read_only: command.read_only,
                schema: command.schema,
                digest: command.digest,
                seen,
            }
        })
        .collect()
}

/// How a kept connection signs in. Nothing of it is secret: what the
/// service's issuer handed out is in the person's vault, and this names
/// which items.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SignsIn {
    /// How the service says its person signs in.
    pub(crate) way: Way,
    /// What this server is known as at the service's issuer, once it made
    /// itself known there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client: Option<Client>,
    /// The items it signs in with, while its person is signed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tokens: Option<Tokens>,
}

/// A connection as kept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Kept {
    pub(crate) id: String,
    pub(crate) address: String,
    #[serde(default)]
    pub(crate) name: String,
    pub(crate) added_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_used_ms: Option<u64>,
    /// Why it is not ready; none while it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) why: Option<Why>,
    pub(crate) state_since_ms: u64,
    pub(crate) sign_in: SignIn,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sign_in_at: Option<String>,
    #[serde(default)]
    pub(crate) commands: Vec<KeptCommand>,
    #[serde(default)]
    pub(crate) commands_read_ms: u64,
    #[serde(default)]
    pub(crate) unusable: u32,
    /// How it signs in, once its service said how; none for a service that
    /// asks for no sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) signs_in: Option<SignsIn>,
}

impl Kept {
    /// What a client reads of it.
    pub(crate) fn view(&self) -> Connection {
        Connection {
            id: self.id.clone(),
            address: self.address.clone(),
            name: self.name.clone(),
            added_at_ms: self.added_ms,
            last_used_at_ms: self.last_used_ms,
            state: self.why.map_or(State::Ready, Why::state),
            state_why: self.why.map(|why| why.words().to_owned()),
            state_since_ms: self.state_since_ms,
            sign_in: self.sign_in,
            sign_in_at: self.sign_in_at.clone(),
            commands: self.commands.iter().map(KeptCommand::view).collect(),
            commands_read_at_ms: self.commands_read_ms,
            unusable: self.unusable,
        }
    }

    /// Stand as `why` says from `now_ms`, keeping since when where nothing
    /// changed.
    pub(crate) fn stand(&mut self, why: Option<Why>, now_ms: u64) {
        if self.why != why {
            self.why = why;
            self.state_since_ms = now_ms;
        }
    }
}

/// Whether `text` could be a connection's id: 32 lowercase hexadecimal
/// digits, so it names a file of the connections' folder and nothing else.
pub(crate) fn is_id(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A new connection's id.
pub(crate) fn new_id() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

fn file(root: &Path, id: &str) -> PathBuf {
    root.join(DIR).join(format!("{id}.json"))
}

fn uses_file(root: &Path, id: &str) -> PathBuf {
    root.join(DIR).join(format!("{id}.uses.json"))
}

/// One call of a command, as written down. Neither its arguments nor its
/// answer are kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KeptUse {
    /// Which call of its connection it is, counted from 1.
    pub(crate) nth: u64,
    pub(crate) at_ms: u64,
    pub(crate) command: String,
    /// The agent that called, by the name its person knows it under; empty
    /// for the person themselves.
    #[serde(default)]
    pub(crate) by: String,
    /// By what leave; none for a call refused for want of one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) leave: Option<Leave>,
    /// How it ended; none for a call whose end was never written down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ended: Option<Ended>,
    /// Why it ended so, in the system's words, when it did not end well.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) why: Option<String>,
}

impl KeptUse {
    pub(crate) fn view(&self) -> Use {
        Use {
            at_ms: self.at_ms,
            command: self.command.clone(),
            by: self.by.clone(),
            leave: self.leave,
            ended: self.ended.unwrap_or(Ended::Failed),
            why: match self.ended {
                Some(_) => self.why.clone(),
                None => Some("How the call ended was not written down".to_owned()),
            },
        }
    }
}

fn read_uses(root: &Path, id: &str) -> io::Result<Vec<KeptUse>> {
    match std::fs::read(uses_file(root, id)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn write_uses(root: &Path, id: &str, uses: &[KeptUse]) -> io::Result<()> {
    let bytes = serde_json::to_vec(uses)?;
    tilo_files::native::atomic::write_creating_parent_with_mode(&uses_file(root, id), &bytes, 0o600)
}

/// The calls the connection `id` names under `root` remembers, oldest
/// first; `None` when no such connection is kept.
pub(crate) fn uses(root: &Path, id: &str) -> io::Result<Option<Vec<KeptUse>>> {
    if one(root, id)?.is_none() {
        return Ok(None);
    }
    read_uses(root, id).map(Some)
}

/// Write down that a command of the connection `id` names under `root` is
/// called, before it is, as `used` says, answering which call it is. A call
/// that cannot be written down is not made. `None` when no such connection
/// is kept.
pub(crate) fn begin_use(root: &Path, id: &str, mut used: KeptUse) -> io::Result<Option<u64>> {
    let held = WRITES.lock_recover();
    let outcome = (|| {
        if one(root, id)?.is_none() {
            return Ok(None);
        }
        let mut written = read_uses(root, id)?;
        used.nth = written.last().map_or(1, |last| last.nth.saturating_add(1));
        let nth = used.nth;
        written.push(used);
        let excess = written.len().saturating_sub(MAX_USES);
        write_uses(root, id, written.get(excess..).unwrap_or_default())?;
        Ok(Some(nth))
    })();
    drop(held);
    outcome
}

/// Write down how the call `nth` of the connection `id` names under `root`
/// ended.
pub(crate) fn end_use(
    root: &Path,
    id: &str,
    nth: u64,
    ended: Ended,
    why: Option<&str>,
) -> io::Result<()> {
    let held = WRITES.lock_recover();
    let outcome = (|| {
        let mut written = read_uses(root, id)?;
        if let Some(call) = written.iter_mut().find(|call| call.nth == nth) {
            call.ended = Some(ended);
            call.why = why.map(str::to_owned);
            write_uses(root, id, &written)?;
        }
        Ok(())
    })();
    drop(held);
    outcome
}

fn read(path: &Path) -> io::Result<Option<Kept>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write(root: &Path, kept: &Kept) -> io::Result<()> {
    let bytes = serde_json::to_vec(kept)?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &file(root, &kept.id),
        &bytes,
        0o600,
    )
}

/// Every connection kept under `root`, oldest first. One that does not read
/// is left out and logged: the others are still the person's.
pub(crate) fn all(root: &Path) -> io::Result<Vec<Kept>> {
    let entries = match std::fs::read_dir(root.join(DIR)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut kept = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let named = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(is_id)
            && path
                .extension()
                .is_some_and(|extension| extension == "json");
        if !named {
            continue;
        }
        match read(&path) {
            Ok(Some(connection)) => kept.push(connection),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "a kept connection does not read and is left out");
            }
        }
    }
    kept.sort_by(|a, b| (a.added_ms, &a.id).cmp(&(b.added_ms, &b.id)));
    Ok(kept)
}

/// The connection `id` names under `root`, when one is kept.
pub(crate) fn one(root: &Path, id: &str) -> io::Result<Option<Kept>> {
    if !is_id(id) {
        return Ok(None);
    }
    read(&file(root, id))
}

/// Why a connection was not added.
#[derive(Debug)]
pub(crate) enum NotAdded {
    /// The person keeps as many as one person keeps.
    Full,
    Storage(io::Error),
}

/// Keep `new` under `root`, unless a connection to its address is kept
/// already: that one is answered, as it is.
pub(crate) fn add(root: &Path, new: Kept) -> Result<Kept, NotAdded> {
    let held = WRITES.lock_recover();
    let outcome = (|| {
        let kept = all(root).map_err(NotAdded::Storage)?;
        if let Some(same) = kept.iter().find(|kept| kept.address == new.address) {
            return Ok(same.clone());
        }
        if kept.len() >= MAX_CONNECTIONS {
            return Err(NotAdded::Full);
        }
        write(root, &new).map_err(NotAdded::Storage)?;
        Ok(new)
    })();
    drop(held);
    outcome
}

/// Change the connection `id` names under `root` as `change` says and keep
/// it, answering it as kept then; `None` when no such connection is kept,
/// also when it was removed meanwhile.
pub(crate) fn update(
    root: &Path,
    id: &str,
    change: impl FnOnce(&mut Kept),
) -> io::Result<Option<Kept>> {
    let held = WRITES.lock_recover();
    let outcome = (|| {
        let Some(mut kept) = one(root, id)? else {
            return Ok(None);
        };
        change(&mut kept);
        write(root, &kept)?;
        Ok(Some(kept))
    })();
    drop(held);
    outcome
}

/// Forget the connection `id` names under `root`, answering it as it was
/// kept; `None` when none was.
pub(crate) fn forget(root: &Path, id: &str) -> io::Result<Option<Kept>> {
    let held = WRITES.lock_recover();
    let outcome = (|| {
        let Some(kept) = one(root, id)? else {
            return Ok(None);
        };
        std::fs::remove_file(file(root, id))?;
        // What it was used for goes with it.
        match std::fs::remove_file(uses_file(root, id)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        Ok(Some(kept))
    })();
    drop(held);
    outcome
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn offered(name: &str, read_only: bool, digest: &str) -> Offered {
        Offered {
            name: name.into(),
            title: None,
            description: String::new(),
            read_only,
            schema: json!({"type": "object"}),
            digest: digest.into(),
        }
    }

    fn a_connection(id: &str, address: &str, added_ms: u64) -> Kept {
        Kept {
            id: id.into(),
            address: address.into(),
            name: "Orchard".into(),
            added_ms,
            last_used_ms: None,
            why: None,
            state_since_ms: added_ms,
            sign_in: SignIn::None,
            sign_in_at: None,
            commands: Vec::new(),
            commands_read_ms: added_ms,
            unusable: 0,
            signs_in: None,
        }
    }

    /// A command is called unasked only while it is marked as only reading and
    /// is defined as its person last saw it: one the service defines anew,
    /// or adds later, asks until the person reads the commands themselves.
    #[test]
    fn what_a_person_did_not_see_asks_whatever_the_service_marks() {
        let first = commands(
            vec![offered("find", true, "a1"), offered("plant", false, "b1")],
            &[],
            true,
        );
        assert!(!first[0].asks() && !first[0].changed());
        assert!(first[1].asks() && !first[1].changed());

        // Read again by the server or an agent: the service now calls plant
        // only reading, and lists a new command marked the same.
        let later = commands(
            vec![
                offered("find", true, "a1"),
                offered("plant", true, "b2"),
                offered("fell", true, "c1"),
            ],
            &first,
            false,
        );
        assert!(!later[0].asks());
        assert!(later[1].asks() && later[1].changed() && later[1].read_only);
        assert!(later[2].asks() && later[2].changed());
        assert!(later[1].view().asks);

        // The person reads them: what they saw now is what holds.
        let seen = commands(
            vec![
                offered("find", true, "a1"),
                offered("plant", true, "b2"),
                offered("fell", true, "c1"),
            ],
            &later,
            true,
        );
        assert!(
            seen.iter()
                .all(|command| !command.asks() && !command.changed())
        );
    }

    /// Connections are kept one file each under the person's root, read
    /// back oldest first, and a name that is no id opens no file.
    #[test]
    fn connections_are_kept_under_the_persons_root_and_named_by_id_alone() {
        let root = tempfile::tempdir().unwrap();
        let (first, second) = (new_id(), new_id());
        assert!(is_id(&first) && first != second);
        add(root.path(), a_connection(&second, "https://b.example/", 20)).unwrap();
        add(root.path(), a_connection(&first, "https://a.example/", 10)).unwrap();
        let kept = all(root.path()).unwrap();
        assert_eq!(
            kept.iter().map(|kept| kept.id.as_str()).collect::<Vec<_>>(),
            [first.as_str(), second.as_str()]
        );
        // The same address again is the connection already kept.
        let again = add(
            root.path(),
            a_connection(&new_id(), "https://a.example/", 30),
        )
        .unwrap();
        assert_eq!(again.id, first);
        assert_eq!(all(root.path()).unwrap().len(), 2);

        for name in [
            "",
            "../vault",
            "..",
            &first.to_uppercase(),
            &format!("{first}0"),
            "a/b",
        ] {
            assert!(!is_id(name), "{name}");
            assert_eq!(one(root.path(), name).unwrap(), None, "{name}");
            assert!(update(root.path(), name, |_| {}).unwrap().is_none());
            assert!(forget(root.path(), name).unwrap().is_none());
        }

        let stood = update(root.path(), &first, |kept| {
            kept.stand(Some(Why::NoAnswer), 40);
        })
        .unwrap()
        .unwrap();
        assert_eq!(stood.view().state, State::Unreachable);
        assert_eq!(stood.view().state_since_ms, 40);
        assert_eq!(
            stood.view().state_why.as_deref(),
            Some("The service did not answer")
        );
        assert!(forget(root.path(), &first).unwrap().is_some());
        // A connection removed is not brought back by a change that was
        // under way.
        assert!(
            update(root.path(), &first, |kept| kept.stand(None, 50))
                .unwrap()
                .is_none()
        );
        assert_eq!(all(root.path()).unwrap().len(), 1);
    }

    /// One person keeps a bounded number.
    #[test]
    fn a_person_keeps_a_bounded_number_of_connections() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..MAX_CONNECTIONS {
            let address = format!("https://service-{index}.example/");
            add(root.path(), a_connection(&new_id(), &address, 1)).unwrap();
        }
        let refused = add(
            root.path(),
            a_connection(&new_id(), "https://one-more.example/", 1),
        );
        assert!(matches!(refused, Err(NotAdded::Full)), "{refused:?}");
    }
}
