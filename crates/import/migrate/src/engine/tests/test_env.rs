//! What the migrate tests land into: one workspace that keeps everything it is asked, and one
//! landing as `tilo migrate start` makes it, for whichever source.
//!
//! Thunderbird's accounts are answered by the same fake, as a real server answers them: the
//! accounts it keeps, as it lists them, never a password.

use anyhow::Result;
use serde_json::Value;

use super::super::state::{Journal, Place, RunState};
use super::super::writer::{Answer, Invocation, Refused, Workspace};
use super::super::{Lander, Stopped, thunderbird};

/// What the person was told and asked during a migration, and whether they said yes.
#[derive(Default)]
pub struct Told {
    pub lines: Vec<String>,
    pub asked: Vec<String>,
    pub yes: bool,
    /// Whether the run is asked to stop from its first item.
    pub stop: bool,
}

/// A workspace that keeps every action and upload it is asked for, and the person's mail
/// accounts as the real workspace answers them.
#[derive(Default)]
pub struct Recorded {
    /// Every call, as the run that made it, the action, and its arguments.
    pub actions: Vec<(String, String, Value)>,
    pub puts: Vec<Vec<u8>>,
    /// Every account the person asked the server to add, as it was asked.
    pub added: Vec<Value>,
    /// The accounts the server keeps, as it lists them: never a password.
    pub kept: Vec<Value>,
    /// The server's refusal of every account added, in its words.
    pub refuses: Option<String>,
    /// The server answers an account added and then does not list it.
    pub forgets: bool,
}

impl Workspace for Recorded {
    fn act(&mut self, run: &Invocation, action: &str, args: Value) -> Result<Answer> {
        self.actions.push((run.id.clone(), action.to_owned(), args));
        Ok(Answer::of(None))
    }

    fn ask(&mut self, action: &str, args: Value) -> Result<Answer> {
        self.actions.push((String::new(), action.to_owned(), args));
        Ok(Answer::of(None))
    }

    fn person(&mut self, path: &str, action: &str, args: Value) -> Result<Value> {
        anyhow::ensure!(path == thunderbird::MAIL_ACCOUNTS, "asked {path}");
        match action {
            "list" => Ok(serde_json::json!({"accounts": self.kept})),
            "add" => {
                self.added.push(args.clone());
                if let Some(why) = &self.refuses {
                    return Err(Refused::coded("refused", why.clone()).into());
                }
                let account = serde_json::json!({
                    "id": args["address"], "address": args["address"],
                    "displayName": args["displayName"],
                    "incomingHost": args["incomingHost"], "incomingPort": args["incomingPort"],
                    "outgoingHost": args["outgoingHost"], "outgoingPort": args["outgoingPort"],
                    "hasSecret": args.get("password").is_some(),
                });
                if !self.forgets {
                    self.kept.push(account.clone());
                }
                Ok(account)
            }
            other => anyhow::bail!("no workspace/mail-accounts#{other}"),
        }
    }

    fn put(&mut self, bytes: &[u8]) -> Result<String> {
        self.puts.push(bytes.to_vec());
        Ok(format!("tilo.content:{}", blake3::hash(bytes).to_hex()))
    }
}

impl Recorded {
    /// The arguments of every call of `action`.
    pub fn of(&self, action: &str) -> Vec<&Value> {
        self.actions
            .iter()
            .filter(|(_, name, _)| name == action)
            .map(|(_, _, args)| args)
            .collect()
    }
}

/// Run `f` with what a migration of `home` into `place` runs with, reaching no workspace and
/// asking nobody for a secret.
pub fn with_cx<T>(
    place: &Place,
    home: &std::path::Path,
    f: impl FnOnce(&super::super::Cx<'_>) -> T,
) -> T {
    let folders = std::collections::BTreeMap::new();
    let running = |_: &str| false;
    let scratch = std::env::temp_dir();
    let open = || -> Result<Box<dyn Workspace>> { anyhow::bail!("this test reaches no workspace") };
    let secret = |_: &str, _: Option<&std::path::Path>| -> Result<zeroize::Zeroizing<String>> {
        anyhow::bail!("nobody types a secret in this test")
    };
    f(&super::super::Cx {
        place,
        running: &running,
        scratch: &scratch,
        home,
        folders: &folders,
        open: &open,
        secret: &secret,
    })
}

/// One landing of `source`, as `tilo migrate start` makes it: the journal it leaves behind, its
/// run's state set by how it ended, and what the person was told and asked.
pub fn land_as(
    place: &Place,
    source: &str,
    invocation: &Invocation,
    workspace: &mut dyn Workspace,
    told: &mut Told,
    land: impl FnOnce(&mut Lander<'_>) -> Result<()>,
) -> Result<Journal> {
    let mut journal = place.journal(source)?;
    journal.brought.clear();
    journal.already_there = 0;
    let lines = std::cell::RefCell::new(Vec::new());
    let asked = std::cell::RefCell::new(Vec::new());
    let mut progress = |line: &str| lines.borrow_mut().push(line.to_owned());
    let stop = told.stop;
    let stopping = || stop;
    let scratch = tempfile::tempdir()?;
    let mut confirm = |question: &str| {
        asked.borrow_mut().push(question.to_owned());
        Ok(told.yes)
    };
    let result = {
        let mut lander = Lander {
            workspace,
            opened: std::collections::BTreeSet::new(),
            landing_in: None,
            invocation,
            journal: &mut journal,
            place,
            source,
            progress: &mut progress,
            stopping: &stopping,
            scratch: scratch.path(),
            confirm: &mut confirm,
        };
        land(&mut lander)
    };
    told.lines.extend(lines.into_inner());
    told.asked.extend(asked.into_inner());
    journal.state = match &result {
        Ok(()) => RunState::Complete,
        Err(error) if error.is::<Stopped>() => RunState::Stopped,
        Err(_) => RunState::Failed,
    };
    place.keep_journal(source, &journal)?;
    result.map(|()| journal)
}
