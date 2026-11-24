//! `tilo migrate`: bring the person's things from this computer and the services they use into
//! their workspace, with no window: the terminal's view of this device's migrations, the actions
//! of provider `so.tilo.migrate` on `devices/<this device>/migrate`
//! ([`tilo_migrate::provider`]), which the device's daemon runs.
//!
//! The sources are listed in one table ([`tilo_migrate::engine::registry::SOURCES`]): folders on
//! this computer, its browsers and Thunderbird, a mail account, Google Drive, `OneDrive`, Dropbox,
//! iCloud Drive, Nextcloud and `WebDAV`, and Outline, Linear and Forgejo. Each step is a command
//! of its own, so an agent can show the person what a source holds before anything moves:
//!
//! ```text
//! tilo migrate sources                         every source, and how to connect each
//! tilo migrate connect gdrive                  sign in (in the browser) where a source needs it
//! tilo migrate review thunderbird              what it holds: accounts, folders, …
//! tilo migrate choose thunderbird ada@work.example
//! tilo migrate start thunderbird               bring the chosen things over
//! tilo migrate status / stop
//! tilo migrate undo <run>                      take a run back
//! ```
//!
//! Each command is one invocation of the step's action, as the person whose account runs it,
//! through the daemon's native endpoint; the Migrations window and the person's agents invoke the
//! same actions, so what one chooses or starts the others see. A run is the invocation of
//! `start`: this command follows it and prints what it brings as it goes. Keys and passwords
//! never pass through the agent: a step that asks for one fails saying so, and the person types it
//! here at a hidden prompt (or names a file holding it), after which the step runs again with it.
//!
//! Connecting a mail account is a decision the person makes, so the action that keeps the account
//! asks them first, as its asked invocation record. This command answers that record where it
//! runs, as their terminal: `--yes` says yes beforehand, and with nobody to ask the account is
//! left unconnected and the run says so. The same question shows in their shell and to an agent's
//! caller.
//!
//! `-w` names a workspace, which is the one this computer's device is in: a migration lands where
//! the device does, and a workspace the device is not in is refused, naming `tilo login`.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeRequest, LineClient, MAX_REMOTE_WAIT, ReceiptStatus,
    RemoteActionTarget, UpdateMark, asked, workspace_invocations,
};
use tilo_actions::{ActionAddress, ActionFailure, InvocationId, InvocationUpdate};
use tilo_migrate::engine;
use tilo_migrate::provider::{PROVIDER, SECRET_NEEDED, resource};

/// How long a command waits for this computer's daemon to answer, its service started.
const REACHING: Duration = Duration::from_secs(30);

#[derive(Subcommand)]
pub enum MigrateCmd {
    /// Every source, what it holds here, and how to connect the ones that need it.
    Sources,
    /// Connect a source that needs signing in: Google Drive, `OneDrive` and Dropbox in the browser
    /// (the link is opened or printed, and this finishes by itself); iCloud, Nextcloud, `WebDAV`,
    /// a mail account (IMAP), Outline, Linear and Forgejo with a password or key typed at a hidden
    /// prompt (or read from --secret-file). It is kept in this computer's vault and never printed.
    Connect {
        /// gdrive, onedrive, dropbox, icloud, nextcloud, webdav, mail, outline, linear or forgejo.
        service: String,
        /// The address: a Nextcloud or `WebDAV` server, Outline's site, Linear's API when not its
        /// own, the Forgejo repository or owner whose issues come over.
        #[arg(long)]
        url: Option<String>,
        /// The account: a mail address, an Apple ID, a Nextcloud or `WebDAV` user name.
        #[arg(long)]
        login: Option<String>,
        /// A file holding the key or password, which the person made (not `--key-file`, which
        /// names the key a hosted workspace is reached with).
        #[arg(long, value_name = "FILE")]
        secret_file: Option<PathBuf>,
    },
    /// List what a source holds, with counts, and keep the review for `choose` and `start`.
    Review {
        /// A source, as `tilo migrate sources` names it (home, gdrive, thunderbird, …).
        source: String,
        /// The Thunderbird profile: one of this computer's by its name, when it has several, or
        /// any profile by the path of its folder or its prefs.js.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Choose what of a reviewed source comes over (or, with --leave-out, what stays).
    Choose {
        /// A source, as `tilo migrate sources` names it (home, gdrive, thunderbird, …).
        source: String,
        /// Items by the key the review lists: a folder, an account's address, `contacts/…`,
        /// `calendar/…`, an Outline collection, a Linear team, a Forgejo repository; for home,
        /// also any folder by its path (`~/code/site`).
        #[arg(required = true)]
        items: Vec<String>,
        /// Leave these out instead.
        #[arg(long)]
        leave_out: bool,
        /// Choose only these: everything else is left out.
        #[arg(long, conflicts_with = "leave_out")]
        only: bool,
    },
    /// Bring the chosen things over, as one run in the workspace; resumes a stopped run.
    Start {
        /// A source, as `tilo migrate sources` names it (home, gdrive, thunderbird, …).
        source: String,
        /// Say yes beforehand to connecting the chosen mail accounts. Without it each account is
        /// asked here, and a caller with nobody to ask leaves it unconnected and says so.
        #[arg(long)]
        yes: bool,
    },
    /// How each source's migration stands.
    Status {
        /// One source; all of them when not given.
        source: Option<String>,
    },
    /// Stop a running migration after the item it is on; `start` resumes it.
    Stop {
        /// A source, as `tilo migrate sources` names it (home, gdrive, thunderbird, …).
        source: String,
    },
    /// Take a run back: everything it brought leaves the workspace, as undoing its History
    /// entry does, and a later `start` brings it again.
    Undo {
        /// The run, by the id `status` names (`migrate-linear-…`), or a source for its last run.
        run: String,
    },
}

/// Run `cmd` on this computer's device; `json` answers as JSON. `workspace` names a workspace
/// with `-w`, which must be one this computer's device is in.
pub fn run(cmd: MigrateCmd, json: bool, workspace: Option<&str>) -> Result<ExitCode> {
    let device = Device::reach()?;
    if let Some(named) = workspace {
        device.in_workspace(named)?;
    }
    let answer = match cmd {
        MigrateCmd::Sources => device.call("sources", Map::new(), json)?,
        MigrateCmd::Connect {
            service,
            url,
            login,
            secret_file,
        } => {
            let mut args = Map::new();
            args.insert("source".into(), json!(service));
            if let Some(url) = url {
                args.insert("url".into(), json!(url));
            }
            if let Some(login) = login {
                args.insert("login".into(), json!(login));
            }
            if let Some(file) = &secret_file {
                args.insert("secret".into(), json!(*secret("", Some(file))?));
            }
            match device.step("connect", args.clone(), json, Asks::Leave)? {
                Ok(answer) => answer,
                Err(failure) => match asked_secret(&failure) {
                    // The person types what it asks for, and it runs again with it.
                    Some(asks) if secret_file.is_none() => {
                        args.insert("secret".into(), json!(*secret(&asks, None)?));
                        device.call("connect", args, json)?
                    }
                    _ => return Err(failed(failure)),
                },
            }
        }
        MigrateCmd::Review { source, profile } => {
            let mut args = sourced(&source);
            if let Some(profile) = profile {
                args.insert("profile".into(), json!(handed_profile(&profile)?));
            }
            device.call("review", args, json)?
        }
        MigrateCmd::Choose {
            source,
            items,
            leave_out,
            only,
        } => {
            let mut args = sourced(&source);
            args.insert("items".into(), json!(items));
            args.insert("leave_out".into(), json!(leave_out));
            args.insert("only".into(), json!(only));
            device.call("choose", args, json)?
        }
        MigrateCmd::Start { source, yes } => {
            let mut args = sourced(&source);
            args.insert("yes".into(), json!(yes));
            if !json {
                eprintln!(
                    "Bringing {source} over; `tilo migrate stop {source}` stops it after the item \
                     it is on."
                );
            }
            device.call_answering("start", args, json, Asks::of(yes))?
        }
        MigrateCmd::Status { source } => {
            let args = source.as_deref().map(sourced).unwrap_or_default();
            device.call("status", args, json)?
        }
        MigrateCmd::Stop { source } => device.call("stop", sourced(&source), json)?,
        MigrateCmd::Undo { run } => {
            let mut args = Map::new();
            args.insert("run".into(), json!(run));
            device.call("undo", args, json)?
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&answer)?);
    } else {
        print!("{}", engine::sentences(&answer));
    }
    let failed = answer.get("state").and_then(Value::as_str) == Some("failed");
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The arguments naming `source`.
fn sourced(source: &str) -> Map<String, Value> {
    let mut args = Map::new();
    args.insert("source".into(), json!(source));
    args
}

/// The Thunderbird profile `profile` as the device reads it: one of its own by name, or a
/// profile folder or prefs.js the person names by a path here, which the device reads from
/// where it is.
fn handed_profile(profile: &str) -> Result<String> {
    let path = Path::new(profile);
    if !path.exists() {
        return Ok(profile.to_owned());
    }
    let path =
        std::fs::canonicalize(path).with_context(|| format!("{} is unreadable", path.display()))?;
    Ok(path.display().to_string())
}

/// What a step that failed `failure` asks the person to type, when it failed for a key or
/// password it was not given.
fn asked_secret(failure: &ActionFailure) -> Option<String> {
    if failure.code != SECRET_NEEDED {
        return None;
    }
    failure
        .details
        .as_ref()?
        .get("asks")?
        .as_str()
        .map(str::to_owned)
}

/// A step's failure, as the person reads it.
fn failed(failure: ActionFailure) -> anyhow::Error {
    anyhow::Error::msg(failure.message)
}

/// This computer's device, whose migrations each command invokes.
struct Device {
    client: LineClient,
    /// The resource the device's migrations are.
    migrations: String,
    /// The workspace this device is in, and its name as the person knows it.
    workspace: String,
    workspace_name: Option<String>,
}

impl Device {
    /// This computer's device, through its daemon's native endpoint, the service started when
    /// none runs.
    ///
    /// # Errors
    /// This computer is not logged in, or its daemon could not be reached.
    fn reach() -> Result<Self> {
        let login = crate::login::Place::here(crate::identity::paths())?;
        let device = crate::login::served_device_key(login.keys())
            .ok_or_else(|| anyhow!(crate::login::NOT_LOGGED_IN))?;
        let home = crate::login::home();
        // A daemon that is there is waited for, not replaced: it may be busy with a change.
        let stream = tilo_api::daemon::this_computer()
            .and_then(|place| place.reach_native(REACHING))
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            client: LineClient::of_stream(stream)?,
            migrations: resource(&device),
            workspace: home
                .as_ref()
                .map(|home| home.workspace.clone())
                .unwrap_or_default(),
            workspace_name: home.and_then(|home| home.workspace_name),
        })
    }

    /// Refuse a workspace this computer's device is not in, naming the command that joins it.
    ///
    /// # Errors
    /// The device is not in a workspace `named` calls by its id or its name.
    fn in_workspace(&self, named: &str) -> Result<()> {
        if in_workspace(named, &self.workspace, self.workspace_name.as_deref()) {
            return Ok(());
        }
        Err(not_in(named))
    }

    /// Invoke `action` with `args` and follow it to its end: its answer, or why it failed.
    fn call(&self, action: &str, args: Map<String, Value>, json: bool) -> Result<Value> {
        self.call_answering(action, args, json, Asks::Leave)
    }

    /// Invoke `action` with `args` and follow it, answering the questions it asks as `asks`
    /// says: its answer, or why it failed.
    fn call_answering(
        &self,
        action: &str,
        args: Map<String, Value>,
        json: bool,
        asks: Asks,
    ) -> Result<Value> {
        self.step(action, args, json, asks)?.map_err(failed)
    }

    /// Invoke `action` with `args` and follow it to its end, saying its progress on stderr
    /// unless `json`: its answer, or the failure it ran into.
    ///
    /// # Errors
    /// The daemon did not answer, or refused the step before it ran.
    fn step(
        &self,
        action: &str,
        args: Map<String, Value>,
        json: bool,
        asks: Asks,
    ) -> Result<std::result::Result<Value, ActionFailure>> {
        let request = InvokeRequest::plain(
            RemoteActionTarget {
                provider: PROVIDER.into(),
                address: ActionAddress::new(self.migrations.clone(), action),
            },
            args,
        );
        let id = request.invocation;
        // The daemon decides the step as whoever connects; the caller here names nobody.
        let caller = Caller::local_person("");
        let receipt = answered(self.client.invoke(&caller, request))?
            .map_err(|error| anyhow!("{action} was refused: {}", error.said()))?;
        self.follow(&caller, id, receipt.status, json, asks)
    }

    /// Follow the invocation `id`, whose receipt said `status`, to its end, answering the
    /// questions it asks as `asks` says.
    fn follow(
        &self,
        caller: &Caller,
        id: InvocationId,
        mut status: ReceiptStatus,
        json: bool,
        asks: Asks,
    ) -> Result<std::result::Result<Value, ActionFailure>> {
        let stopped = || anyhow!("this computer's daemon stopped answering");
        let mut seen = UpdateMark::Accepted;
        loop {
            match status {
                ReceiptStatus::Accepted => {}
                ReceiptStatus::Completed { output } => {
                    return Ok(Ok(output.value.unwrap_or_default()));
                }
                ReceiptStatus::Failed { error } => return Ok(Err(error)),
                ReceiptStatus::Rejected { error } => bail!("{}", error.said()),
                ReceiptStatus::Cancelled { message } => {
                    return Ok(Ok(json!({
                        "said": message.unwrap_or_else(|| "it was stopped".into()),
                    })));
                }
            }
            // Asked for before looking, so a change between the two is never missed.
            let change = self.client.next_change(caller, &id).ok_or_else(stopped)?;
            let state = self.client.observe(caller, &id).ok_or_else(stopped)?;
            if UpdateMark::of(&state.update) == seen {
                if asks.answers() {
                    // A run that waits for its person asks in a record of its own: the
                    // terminal answers it, as any other client of the workspace does.
                    self.answer_asks(asks, json)?;
                    let _ = change.wait_timeout(LOOK_AGAIN);
                } else {
                    // A wait the daemon let pass unanswered is looked at again.
                    let _ = change.wait_timeout(MAX_REMOTE_WAIT.saturating_add(REACHING));
                }
                continue;
            }
            seen = UpdateMark::of(&state.update);
            if let InvocationUpdate::Progress {
                message: Some(line),
                ..
            } = &state.update
                && !json
            {
                eprintln!("{line}");
            }
            status = state.receipt().status;
        }
    }
}

/// Whether `named` is the workspace this computer's device is in, by its id or the name its
/// person knows it under.
fn in_workspace(named: &str, workspace: &str, name: Option<&str>) -> bool {
    named == workspace
        || name.is_some_and(|name| name.eq_ignore_ascii_case(named))
}

/// The refusal of a workspace this computer's device is not in: the command that joins it is the
/// one to name, so a person has the step to take.
fn not_in(named: &str) -> anyhow::Error {
    anyhow!(
        "this computer's device is not in the workspace {named:?}: join it with the invitation \
         link a member sends you, `tilo login <invitation link>` (a server of yours takes it in \
         with `tilo login --server`), then run `tilo migrate -w {named}`"
    )
}

/// How the terminal answers the questions a run asks: say yes beforehand (`--yes`), ask the
/// person here, leave them to someone else, or (with nobody to ask) say no, so the account stays
/// unconnected and the run says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asks {
    /// Answer nobody: another client of the workspace (the person's shell) does.
    Leave,
    /// Say yes to every question, as `--yes` does.
    Approve,
    /// Say no: nobody is at a terminal to ask.
    Decline,
    /// Put each question to the person at this terminal.
    Prompt,
}

impl Asks {
    /// How `tilo migrate start` answers, given its `--yes` and whether a person is at a terminal.
    fn of(yes: bool) -> Self {
        if yes {
            return Self::Approve;
        }
        if std::io::stdin().is_terminal() || std::env::var_os("TILO_ASK_TTY").is_some() {
            Self::Prompt
        } else {
            Self::Decline
        }
    }

    /// Whether the terminal answers the run's questions at all.
    const fn answers(self) -> bool {
        !matches!(self, Self::Leave)
    }
}

/// How long a run that may ask is waited on before its questions are looked for again.
const LOOK_AGAIN: Duration = Duration::from_millis(250);

impl Device {
    /// Answer the questions this run asks, as `asks` says: each is a record in the workspace,
    /// answered with its own `resolve`, as the person's shell answers it.
    fn answer_asks(&self, asks: Asks, json: bool) -> Result<()> {
        for question in self.questions()? {
            if !connects_a_mail_account(&question) {
                continue;
            }
            if !json && asks != Asks::Prompt {
                eprintln!("{}", question_said(&question));
            }
            let approve = match asks {
                Asks::Approve => true,
                Asks::Decline => false,
                Asks::Prompt => asked_at_terminal(&question, json)?,
                Asks::Leave => continue,
            };
            if let Err(error) = self.answer(&question, approve) {
                // Another client answered it first: the run reads their answer.
                eprintln!("the answer did not go through: {error:#}");
                continue;
            }
            if !json {
                eprintln!(
                    "{}",
                    if approve {
                        "You said yes: it is connected."
                    } else {
                        "You said no: it stays unconnected."
                    }
                );
            }
        }
        Ok(())
    }

    /// The questions that wait for this person, oldest first.
    fn questions(&self) -> Result<Vec<Value>> {
        let request = InvokeRequest::plain(
            RemoteActionTarget {
                provider: asked::PROVIDER.into(),
                address: ActionAddress::new(asked::WAITING, asked::LIST),
            },
            Map::new(),
        );
        let caller = Caller::local_person("");
        let receipt = answered(self.client.invoke(&caller, request))?
            .map_err(|error| anyhow!("the questions could not be read: {}", error.said()))?;
        let ReceiptStatus::Completed { output } = receipt.status else {
            return Ok(Vec::new());
        };
        Ok(output
            .value
            .as_ref()
            .and_then(|value| value.get("questions"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Answer `question`'s record with `approve`.
    fn answer(&self, question: &Value, approve: bool) -> Result<()> {
        let Some(record) = question
            .pointer("/record/record")
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        let mut args = Map::new();
        args.insert(
            "resolution".into(),
            json!(if approve { "approve" } else { "decline" }),
        );
        let request = InvokeRequest::plain(
            RemoteActionTarget {
                provider: workspace_invocations::PROVIDER.into(),
                address: ActionAddress::new(record, asked::RESOLVE),
            },
            args,
        );
        let caller = Caller::local_person("");
        answered(self.client.invoke(&caller, request))?
            .map_err(|error| anyhow!("the answer was refused: {}", error.said()))?;
        Ok(())
    }
}

/// Whether `question` asks to keep a mail account: the only question a migration's run asks.
fn connects_a_mail_account(question: &Value) -> bool {
    question
        .get("action")
        .and_then(Value::as_str)
        .is_some_and(|action| action.ends_with("#add"))
}

/// `question` as the person reads it before answering: who asks, in their words, and what is
/// decided.
fn question_said(question: &Value) -> String {
    let mut said = String::new();
    let words = question
        .get("words")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if words.trim().is_empty() {
        let verb = question
            .get("verb")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let on = question
            .get("on")
            .and_then(Value::as_str)
            .unwrap_or_default();
        said.push_str(verb);
        if !on.is_empty() {
            said.push(' ');
            said.push_str(on);
        }
    } else {
        said.push_str(words);
    }
    if let Some(agent) = question
        .pointer("/by/agent/name")
        .and_then(Value::as_str)
    {
        said.push_str(&format!(" (asked by {agent})"));
    }
    for fact in question
        .get("facts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let label = fact.get("label").and_then(Value::as_str).unwrap_or("");
        let value = fact.get("value").and_then(Value::as_str).unwrap_or("");
        said.push_str(&format!("\n  {label}: {value}"));
    }
    said
}

/// Put `question` to the person at this terminal; `false` when nobody is there to ask.
fn asked_at_terminal(question: &Value, json: bool) -> Result<bool> {
    if !json {
        eprintln!("{}", question_said(question));
    }
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok();
    let Some(tty) = tty.as_mut() else {
        eprintln!(
            "Nobody can be asked here, so the account stays unconnected. Run this in your \
             terminal, or pass --yes to say yes beforehand."
        );
        return Ok(false);
    };
    ask_yes(tty)
}

/// One visible line the person types on `tty`: yes for `y`, `yes` or nothing.
fn ask_yes(tty: &mut std::fs::File) -> Result<bool> {
    use std::io::{BufRead as _, Write as _};
    write!(tty, "Connect it? [Y/n] ")?;
    tty.flush()?;
    let mut said = String::new();
    std::io::BufReader::new(&*tty).read_line(&mut said)?;
    let said = said.trim();
    Ok(said.is_empty() || said == "y" || said == "yes" || said == "Y" || said == "Yes")
}

/// What the daemon answers to `pending`.
fn answered<T: Send + 'static>(pending: tilo_actions::remote::Pending<T>) -> Result<T> {
    pending
        .wait_timeout(MAX_REMOTE_WAIT.saturating_add(REACHING))
        .ok()
        .flatten()
        .ok_or_else(|| anyhow!("this computer's daemon did not answer"))
}

/// A secret the person types, unseen, or reads from a file they name; never an argument.
fn secret(prompt: &str, file: Option<&Path>) -> Result<zeroize::Zeroizing<String>> {
    if let Some(file) = file {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("{} is unreadable", file.display()))?;
        return Ok(zeroize::Zeroizing::new(text.trim().to_owned()));
    }
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()
        .filter(|_| std::io::stdin().is_terminal() || std::env::var_os("TILO_ASK_TTY").is_some());
    let Some(tty) = tty.as_mut() else {
        bail!(
            "the key is typed by the person, never passed by an agent: ask them to run this in their terminal, or to save the key in a file and pass --secret-file"
        );
    };
    read_hidden(tty, prompt)
}

/// One line the person types on the terminal `tty` after `prompt`, not shown as it is typed.
pub(crate) fn read_hidden(
    tty: &mut std::fs::File,
    prompt: &str,
) -> Result<zeroize::Zeroizing<String>> {
    use rustix::termios;
    use std::io::{BufRead as _, Write as _};
    write!(tty, "{prompt}")?;
    tty.flush()?;
    let before = termios::tcgetattr(&*tty).ok();
    if let Some(before) = &before {
        let mut hidden = before.clone();
        hidden.local_modes.remove(termios::LocalModes::ECHO);
        let _ = termios::tcsetattr(&*tty, termios::OptionalActions::Now, &hidden);
    }
    let mut line = zeroize::Zeroizing::new(String::new());
    let read = std::io::BufReader::new(&*tty).read_line(&mut line);
    if let Some(before) = &before {
        let _ = termios::tcsetattr(&*tty, termios::OptionalActions::Now, before);
    }
    writeln!(tty)?;
    read?;
    Ok(zeroize::Zeroizing::new(line.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use super::*;

    const ENTERED: &str = "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b";

    /// A workspace the device is in is named by its id or by the name the person knows it under;
    /// one it is not in is not, and the refusal names the command that joins it.
    #[test]
    fn a_workspace_the_device_is_in_is_named_by_id_or_name() {
        assert!(in_workspace(ENTERED, ENTERED, Some("Studio")));
        assert!(in_workspace("studio", ENTERED, Some("Studio")));
        assert!(!in_workspace("Lab", ENTERED, Some("Studio")));
        assert!(!in_workspace("studio", ENTERED, None));

        let refusal = format!("{}", not_in("Lab"));
        assert!(refusal.contains("tilo login"), "{refusal}");
        assert!(refusal.contains("Lab"), "{refusal}");
    }

    /// `--yes` says yes without asking; with nobody at a terminal to ask and no `--yes`, the
    /// answer is no, so the account stays unconnected and the run says so.
    #[test]
    fn the_terminal_says_yes_beforehand_or_leaves_it_unconnected() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_ASK_TTY");
        assert_eq!(Asks::of(true), Asks::Approve);
        assert_eq!(Asks::of(false), Asks::Decline, "nobody is here to ask");
        env.set("TILO_ASK_TTY", "1");
        assert_eq!(Asks::of(false), Asks::Prompt, "a person is at a terminal");
    }

    /// Only the question that keeps a mail account is a migration's: the terminal answers that
    /// one, and leaves another app's question to its own caller.
    #[test]
    fn only_keeping_a_mail_account_is_a_migrations_question() {
        assert!(connects_a_mail_account(
            &json!({"action": "workspace/mail-accounts#add"})
        ));
        assert!(!connects_a_mail_account(
            &json!({"action": "workspace/mail-accounts#list"})
        ));
        assert!(!connects_a_mail_account(&json!({})));
    }

    /// The question reads as the person answers it: who asks, what it is on, and what is decided.
    #[test]
    fn a_question_reads_as_the_person_answers_it() {
        let said = question_said(&json!({
            "action": "workspace/mail-accounts#add",
            "verb": "connect",
            "on": "ada@example.com",
            "facts": [
                {"label": "receives from", "value": "imap.example.com:993"},
                {"label": "signs in with", "value": "ada@example.com"},
            ],
        }));
        assert!(said.contains("connect ada@example.com"), "{said}");
        assert!(said.contains("imap.example.com:993"), "{said}");
        assert!(said.contains("signs in with"), "{said}");
    }
}
