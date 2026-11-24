//! The services a person connected by their address: what this server does
//! to look at one, keep it, sign in to it and call what it offers.
//!
//! A person gives an address. The server connects to the service there as
//! the Model Context Protocol connects to one over streamable HTTP
//! ([`mcp`]), through the one guarded way it sends anything to an address a
//! person named ([`wire`]), reads the name the service gives itself and the
//! commands it lists ([`said`]), and keeps that under the person's own root
//! ([`kept`]), beside their vault. Where the service wants a sign-in, the
//! person signs in by the service's own flow and what it hands out goes
//! into their vault ([`sign_in`]). Nothing here knows who is asking: the
//! workspaces' action service decides that before any of it is reached.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::remote::person_connections::{
    Call, Called, Connection, Connections, Ended, Leave, Looked, SignIn, SignInStep, Uses,
};
use tilo_lock::MutexRecover as _;

mod address;
mod kept;
mod mcp;
mod said;
mod sign_in;
#[cfg(test)]
pub(crate) mod stand_in;
#[cfg(test)]
mod tests;
pub(crate) mod wire;

use address::Address;
use kept::{Kept, KeptUse, NotAdded, SignsIn, Why};
use mcp::{Answered, Session};
use said::Offered;
use sign_in::{NotRenewed, Tokens};

use crate::workspaces::vault::VaultAt;
use wire::Problem;
pub(crate) use wire::Reach;

/// How many times in a minute one person's server asks services for them.
/// Looking at an address makes the server send a request there, whoever
/// suggested the address.
const ASKS_A_MINUTE: usize = 30;
const MINUTE: Duration = Duration::from_secs(60);

/// When each person's services were asked in the last minute, by the
/// person's root.
static ASKED: Mutex<BTreeMap<PathBuf, VecDeque<tilo_clock::time::Instant>>> =
    Mutex::new(BTreeMap::new());

/// One sign-in is renewed at a time: an issuer that hands out a new renewal
/// with each one takes the old only once.
static RENEWING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const NOT_KEPT: &str = "That connection is not kept here";
/// What an agent is told of a command it may not call by itself.
pub(crate) const ASKS_FIRST: &str =
    "That command asks your person first, and asking them is not offered here yet";
/// What a call its person allowed is refused with where the service defines
/// the command otherwise than when they were asked.
pub(crate) const DEFINED_ANEW: &str = "The service defines that command otherwise than when your person was asked. Nothing was called, and they are asked again when it is called next";
const NO_SUCH_COMMAND: &str = "That connection offers no such command";
const SAID_IT_FAILED: &str = "The service said the command failed";
const CALL_NOT_TAKEN: &str = "The service did not take the call as it was made";
/// The most arguments one call carries, as JSON.
const MAX_ARGUMENT_BYTES: usize = 256 * 1024;

/// Who asks a service through a connection.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Calling<'a> {
    /// The person the connection is of, themselves.
    Person,
    /// An agent acting for them, by the name they know it under.
    Agent(&'a str),
    /// An agent acting for them whose call its person allowed, that once
    /// or for good: the call of a command as `digest` names its definition.
    Allowed {
        by: &'a str,
        leave: Leave,
        digest: &'a str,
    },
}

impl Calling<'_> {
    /// The name a use is written down under: empty for the person.
    const fn by(&self) -> &str {
        match self {
            Self::Person => "",
            Self::Agent(name) | Self::Allowed { by: name, .. } => name,
        }
    }

    /// By what leave it calls `command`; none where it does not call it by
    /// itself. What its person allowed is the command as they were shown
    /// it: one defined otherwise since is not it.
    fn leave_for(&self, command: &kept::KeptCommand) -> Option<Leave> {
        match self {
            Self::Person => Some(Leave::Person),
            Self::Agent(_) if !command.asks() => Some(Leave::ReadOnly),
            Self::Allowed { leave, digest, .. } if command.digest == *digest => Some(*leave),
            Self::Agent(_) | Self::Allowed { .. } => None,
        }
    }

    /// What a call it has no leave for is refused with.
    const fn unasked(&self) -> &'static str {
        match self {
            Self::Allowed { .. } => DEFINED_ANEW,
            Self::Person | Self::Agent(_) => ASKS_FIRST,
        }
    }
}

/// One command of a kept connection, as whoever decides a call of it reads
/// it.
pub(crate) struct Callable {
    /// The site the connection's address names: the system's word for the service.
    pub(crate) site: String,
    /// Whether an agent asks its person before it calls the command.
    pub(crate) asks: bool,
    /// What names the command's definition as the service last listed it.
    pub(crate) digest: String,
}

/// The command `command` of the connection `id` names under `root`.
pub(crate) fn callable(
    root: &Path,
    reach: &Reach,
    id: &str,
    command: &str,
) -> Result<Callable, String> {
    let kept = kept_one(root, id)?;
    let found = kept
        .commands
        .iter()
        .find(|kept| kept.name == command)
        .ok_or(NO_SUCH_COMMAND)?;
    Ok(Callable {
        site: site(reach, &kept.address)?,
        asks: found.asks(),
        digest: found.digest.clone(),
    })
}

/// The site `address` names, where this server calls it: what the system
/// calls a service by, whatever the service calls itself.
pub(crate) fn site(reach: &Reach, address: &str) -> Result<String, String> {
    Address::parse(address, reach.destinations)
        .map(|address| address::site_of(address.url()))
        .map_err(|not| not.words().to_owned())
}

/// Whether the service of `kept` still defines `command` as `digest` names
/// it, read from the service before a call its person allowed is sent: they
/// allowed what they were shown. A command defined otherwise now is kept as
/// changed and is not called.
async fn defined_as(
    (root, vault): (&Path, VaultAt<'_>),
    reach: &Reach,
    (kept, address): (&Kept, &Address),
    (command, digest): (&str, &str),
    by: &str,
) -> Result<(), (Ended, &'static str)> {
    let read = match ask((root, vault), reach, (kept, address), by, &Asking::Commands).await {
        Ok(Answer::Commands(read)) => read,
        Ok(Answer::Called(_)) => return Err((Ended::Failed, CALL_NOT_TAKEN)),
        Err(problem) => return Err((Ended::Failed, why(&problem).words())),
    };
    if read
        .offered
        .iter()
        .any(|offered| offered.name == command && offered.digest == digest)
    {
        return Ok(());
    }
    let now = tilo_clock::unix_now_ms();
    let said = kept::update(root, &kept.id, |kept| {
        kept.unusable = read.unusable;
        kept.commands = kept::commands(read.offered, &kept.commands, false);
        kept.commands_read_ms = now;
    });
    if let Err(error) = said {
        tracing::warn!(%error, "what a service lists now could not be kept with its connection");
    }
    Err((Ended::Refused, DEFINED_ANEW))
}

/// Whether the server asks one more service for the person at `root` now.
fn paced(root: &Path) -> Result<(), String> {
    let now = tilo_clock::now();
    let mut asked = ASKED.lock_recover();
    asked.retain(|_, times| {
        times.retain(|at| now.saturating_duration_since(*at) < MINUTE);
        !times.is_empty()
    });
    let times = asked.entry(root.to_path_buf()).or_default();
    let free = times.len() < ASKS_A_MINUTE;
    if free {
        times.push_back(now);
    }
    drop(asked);
    if free {
        Ok(())
    } else {
        Err("Too many services were asked in the last minute. Try again in a moment".into())
    }
}

/// Why a connection is not ready, for what went wrong asking its service.
const fn why(problem: &Problem) -> Why {
    match problem {
        Problem::NotCalled => Why::NotCalled,
        Problem::NoAnswer => Why::NoAnswer,
        Problem::SentOn => Why::SentOn,
        Problem::SignIn { .. } => Why::WantsSignIn,
        Problem::TurnedAway => Why::TurnedAway,
        Problem::Unreadable | Problem::Said => Why::Unreadable,
    }
}

fn unavailable(error: &dyn std::fmt::Display) -> String {
    tracing::warn!(%error, "a person's connections could not be read or kept");
    "The connections are unavailable".into()
}

fn kept_one(root: &Path, id: &str) -> Result<Kept, String> {
    kept::one(root, id)
        .map_err(|error| unavailable(&error))?
        .ok_or_else(|| NOT_KEPT.to_owned())
}

/// What a service said it is and offers.
struct Read {
    name: String,
    offered: Vec<Offered>,
    unusable: u32,
}

/// What is asked of a service.
enum Asking {
    /// What it is and what it offers.
    Commands,
    /// One of its commands, with its arguments as given.
    Call(String, Map<String, Value>),
}

/// What a service answered.
enum Answer {
    Commands(Read),
    Called(Answered),
}

/// Greet the service at `address`, showing `bearer` where the connection
/// has a sign-in, and ask it `asking`.
async fn once(
    reach: &Reach,
    address: &Address,
    bearer: Option<&str>,
    asking: &Asking,
) -> Result<Answer, Problem> {
    let (mut session, told) = Session::open(reach, address, bearer).await?;
    match asking {
        Asking::Commands => {
            let listed = session.commands().await?;
            Ok(Answer::Commands(Read {
                name: told.name,
                offered: listed.offered,
                unusable: listed.unusable,
            }))
        }
        Asking::Call(command, arguments) => Ok(Answer::Called(
            session.call(command, arguments.clone()).await?,
        )),
    }
}

/// The sign-in of `kept` is over: take what it signed in with out of the
/// vault under `root`, and let the connection say so.
async fn ran_out((root, vault): (&Path, VaultAt<'_>), kept: &Kept, tokens: &Tokens) -> Problem {
    sign_in::forget(vault, tokens).await;
    let now = tilo_clock::unix_now_ms();
    let said = kept::update(root, &kept.id, |kept| {
        if let Some(signs) = &mut kept.signs_in {
            signs.tokens = None;
        }
        kept.sign_in = SignIn::Needed;
        kept.stand(Some(Why::SignInRanOut), now);
    });
    if let Err(error) = said {
        tracing::warn!(%error, "a connection whose sign-in ran out could not say so");
    }
    Problem::SignIn { challenge: None }
}

/// Renew the sign-in of `kept`, unless another request renewed it while
/// this one waited: the items the connection signs in with from now on.
async fn renewed(
    (root, vault): (&Path, VaultAt<'_>),
    reach: &Reach,
    (kept, address): (&Kept, &Address),
    (signs, tokens): (&SignsIn, &Tokens),
    by: &str,
) -> Result<Tokens, Problem> {
    let held = RENEWING.lock().await;
    let now_kept = kept::one(root, &kept.id)
        .ok()
        .flatten()
        .and_then(|kept| kept.signs_in)
        .and_then(|signs| signs.tokens);
    let outcome = match (now_kept, &signs.client) {
        (Some(newer), _) if newer != *tokens => Ok(newer),
        (None, _) | (_, None) => Err(ran_out((root, vault), kept, tokens).await),
        (Some(_), Some(client)) => {
            match sign_in::renew(vault, reach, address, (&signs.way, client), tokens, by).await {
                Ok(renewed) => {
                    let keeps = renewed.clone();
                    let said = kept::update(root, &kept.id, |kept| {
                        if let Some(signs) = &mut kept.signs_in {
                            signs.tokens = Some(keeps);
                        }
                    });
                    if matches!(said, Ok(Some(_))) {
                        Ok(renewed)
                    } else {
                        // A connection removed meanwhile keeps no sign-in.
                        sign_in::forget(vault, &renewed).await;
                        Err(Problem::NotCalled)
                    }
                }
                Err(NotRenewed::RanOut) => Err(ran_out((root, vault), kept, tokens).await),
                Err(NotRenewed::NotNow(problem)) => Err(problem),
            }
        }
    };
    drop(held);
    outcome
}

/// Ask the service of `kept` as its connection signs in: with nothing where
/// it has no sign-in, else with what its person's vault keeps for it,
/// renewed once where the service no longer takes that.
async fn ask(
    (root, vault): (&Path, VaultAt<'_>),
    reach: &Reach,
    (kept, address): (&Kept, &Address),
    by: &str,
    asking: &Asking,
) -> Result<Answer, Problem> {
    let signed = kept
        .signs_in
        .as_ref()
        .and_then(|signs| Some((signs, signs.tokens.as_ref()?)));
    let Some((signs, tokens)) = signed else {
        return once(reach, address, None, asking).await;
    };
    let lapsed = tokens
        .expires_ms
        .is_some_and(|until| until <= tilo_clock::unix_now_ms());
    if !lapsed && let Some(shown) = sign_in::bearer(vault, address, tokens, by).await {
        let first = once(reach, address, Some(shown.secret.as_str()), asking).await;
        if !matches!(first, Err(Problem::SignIn { .. })) {
            return first;
        }
    }
    let tokens = renewed((root, vault), reach, (kept, address), (signs, tokens), by).await?;
    let shown = sign_in::bearer(vault, address, &tokens, by)
        .await
        .ok_or(Problem::SignIn { challenge: None })?;
    let again = once(reach, address, Some(shown.secret.as_str()), asking).await;
    if matches!(again, Err(Problem::SignIn { .. })) {
        // A service that turns a sign-in away the moment it was renewed
        // takes it no more.
        return Err(ran_out((root, vault), kept, &tokens).await);
    }
    again
}

/// Every connection kept under `root`.
pub(crate) fn list(root: &Path) -> Result<Connections, String> {
    let kept = kept::all(root).map_err(|error| unavailable(&error))?;
    Ok(Connections {
        connections: kept.iter().map(Kept::view).collect(),
    })
}

/// What the service at `address` says it is. Nothing is kept.
pub(crate) async fn look(root: &Path, reach: &Reach, address: &str) -> Result<Looked, String> {
    let address =
        Address::parse(address, reach.destinations).map_err(|not| not.words().to_owned())?;
    paced(root)?;
    let mut looked = Looked {
        address: address.as_str().to_owned(),
        name: String::new(),
        needs_sign_in: false,
        sign_in_at: None,
        commands: None,
        unusable: 0,
    };
    match once(reach, &address, None, &Asking::Commands).await {
        Ok(Answer::Commands(read)) => {
            looked.name = read.name;
            looked.unusable = read.unusable;
            let commands = kept::commands(read.offered, &[], true);
            looked.commands = Some(commands.iter().map(kept::KeptCommand::view).collect());
        }
        Ok(Answer::Called(_)) => {}
        Err(Problem::SignIn { challenge }) => {
            looked.needs_sign_in = true;
            if let Ok(way) = sign_in::discover(reach, &address, challenge.as_deref()).await {
                looked.sign_in_at = way.site();
                looked.name = way.name.unwrap_or_default();
            }
        }
        Err(problem) => return Err(why(&problem).words().to_owned()),
    }
    Ok(looked)
}

/// Connect the service at `address` for the person whose root `root` is: a
/// service that answers is kept with what it said, one that wants a sign-in
/// is kept as needing one, and one that did not answer is not kept.
pub(crate) async fn add(root: &Path, reach: &Reach, address: &str) -> Result<Connection, String> {
    let address =
        Address::parse(address, reach.destinations).map_err(|not| not.words().to_owned())?;
    paced(root)?;
    let now = tilo_clock::unix_now_ms();
    let mut new = Kept {
        id: kept::new_id(),
        address: address.as_str().to_owned(),
        name: String::new(),
        added_ms: now,
        last_used_ms: None,
        why: None,
        state_since_ms: now,
        sign_in: SignIn::None,
        sign_in_at: None,
        commands: Vec::new(),
        commands_read_ms: now,
        unusable: 0,
        signs_in: None,
    };
    match once(reach, &address, None, &Asking::Commands).await {
        Ok(Answer::Commands(read)) => {
            new.name = read.name;
            new.unusable = read.unusable;
            // The person adds it themselves: what it offers now is what
            // they saw.
            new.commands = kept::commands(read.offered, &[], true);
        }
        Ok(Answer::Called(_)) => {}
        Err(Problem::SignIn { challenge }) => {
            new.why = Some(Why::WantsSignIn);
            new.sign_in = SignIn::Needed;
            if let Ok(way) = sign_in::discover(reach, &address, challenge.as_deref()).await {
                new.sign_in_at = way.site();
                new.name = way.name.clone().unwrap_or_default();
                new.signs_in = Some(SignsIn {
                    way,
                    client: None,
                    tokens: None,
                });
            }
        }
        Err(problem) => return Err(why(&problem).words().to_owned()),
    }
    match kept::add(root, new) {
        Ok(kept) => Ok(kept.view()),
        Err(NotAdded::Full) => Err(format!(
            "A person keeps at most {} connections. Remove one first",
            kept::MAX_CONNECTIONS
        )),
        Err(NotAdded::Storage(error)) => Err(unavailable(&error)),
    }
}

/// Forget the connection `id` names under `root`, answering what remains.
/// What it signed in with leaves the vault with it.
pub(crate) async fn remove(
    (root, vault): (&Path, VaultAt<'_>),
    id: &str,
) -> Result<Connections, String> {
    let was = kept::forget(root, id)
        .map_err(|error| unavailable(&error))?
        .ok_or_else(|| NOT_KEPT.to_owned())?;
    if let Some(tokens) = was.signs_in.and_then(|signs| signs.tokens) {
        sign_in::forget(vault, &tokens).await;
    }
    list(root)
}

/// Read the commands of the connection `id` names under `root` again, for
/// `calling`. Where its person themselves is reading, what the service
/// lists now is what they saw; otherwise a command defined anew asks until
/// they did. A service that cannot be asked leaves the connection as it was
/// read last, and says how it stands.
pub(crate) async fn refresh(
    (root, vault): (&Path, VaultAt<'_>),
    reach: &Reach,
    id: &str,
    calling: Calling<'_>,
) -> Result<Connection, String> {
    let kept = kept_one(root, id)?;
    let signed_in = kept
        .signs_in
        .as_ref()
        .is_some_and(|signs| signs.tokens.is_some());
    let read = match Address::parse(&kept.address, reach.destinations) {
        // An address that was called when it was added and is not called
        // now: the leave it was added under is gone.
        Err(_) => Err(Problem::NotCalled),
        Ok(address) => {
            paced(root)?;
            ask(
                (root, vault),
                reach,
                (&kept, &address),
                calling.by(),
                &Asking::Commands,
            )
            .await
        }
    };
    let seen = matches!(calling, Calling::Person);
    let now = tilo_clock::unix_now_ms();
    let changed = kept::update(root, id, |kept| match read {
        Ok(Answer::Commands(read)) => {
            if !read.name.is_empty() {
                kept.name = read.name;
            }
            kept.unusable = read.unusable;
            kept.commands = kept::commands(read.offered, &kept.commands, seen);
            kept.commands_read_ms = now;
            kept.sign_in = if signed_in {
                SignIn::SignedIn
            } else {
                SignIn::None
            };
            kept.stand(None, now);
        }
        Ok(Answer::Called(_)) => {}
        Err(Problem::SignIn { .. }) => {
            kept.sign_in = SignIn::Needed;
            // A service that took a sign-in and takes it no more says so,
            // until its person signed in again.
            let why = if signed_in || kept.why == Some(Why::SignInRanOut) {
                Why::SignInRanOut
            } else {
                Why::WantsSignIn
            };
            kept.stand(Some(why), now);
        }
        Err(problem) => kept.stand(Some(why(&problem)), now),
    });
    changed
        .map_err(|error| unavailable(&error))?
        .map(|kept| kept.view())
        .ok_or_else(|| NOT_KEPT.to_owned())
}

/// What the connection `id` names under `root` was used for.
pub(crate) fn uses(root: &Path, id: &str) -> Result<Uses, String> {
    let uses = kept::uses(root, id)
        .map_err(|error| unavailable(&error))?
        .ok_or_else(|| NOT_KEPT.to_owned())?;
    Ok(Uses {
        uses: uses.iter().map(KeptUse::view).collect(),
    })
}

/// Call a command of a connection kept under `root`, for `calling`.
///
/// The person calls any command their connection offers. An agent calls one
/// the service marks as only reading and defines as its person last saw it;
/// any other it does not call by itself, and calls where its person allowed
/// that call ([`Calling::Allowed`]) while the service still defines the
/// command as they were shown it. The call is written down before it
/// is sent, with who called and by what leave, and how it ended after; its
/// arguments go to the service exactly as given, and neither they nor the
/// answer are kept.
pub(crate) async fn call(
    (root, vault): (&Path, VaultAt<'_>),
    reach: &Reach,
    calling: Calling<'_>,
    call: Call,
) -> Result<Called, String> {
    let id = call.connection.as_str();
    let kept = kept_one(root, id)?;
    let command = kept
        .commands
        .iter()
        .find(|command| command.name == call.command)
        .ok_or(NO_SUCH_COMMAND)?;
    let leave = calling.leave_for(command);
    let fits = serde_json::to_vec(&call.arguments)
        .is_ok_and(|arguments| arguments.len() <= MAX_ARGUMENT_BYTES);
    let address = Address::parse(&kept.address, reach.destinations);
    let refused = match (leave, fits, &address) {
        (None, _, _) => Some(calling.unasked()),
        (_, false, _) => Some("Those arguments are larger than one call carries"),
        (_, _, Err(_)) => Some(Why::NotCalled.words()),
        _ => None,
    };
    let now = tilo_clock::unix_now_ms();
    let used = KeptUse {
        nth: 0,
        at_ms: now,
        command: call.command.clone(),
        by: calling.by().to_owned(),
        leave,
        ended: refused.map(|_| Ended::Refused),
        why: refused.map(str::to_owned),
    };
    let nth = kept::begin_use(root, id, used)
        .map_err(|error| unavailable(&error))?
        .ok_or_else(|| NOT_KEPT.to_owned())?;
    let (None, Ok(address)) = (refused, address) else {
        return Err(refused.unwrap_or(NOT_KEPT).to_owned());
    };
    paced(root)?;
    if let Calling::Allowed { digest, by, .. } = calling
        && let Err((ended, why)) = defined_as(
            (root, vault),
            reach,
            (&kept, &address),
            (&call.command, digest),
            by,
        )
        .await
    {
        kept::end_use(root, id, nth, ended, Some(why)).map_err(|error| unavailable(&error))?;
        return Err(why.to_owned());
    }
    let signed_in = kept
        .signs_in
        .as_ref()
        .is_some_and(|signs| signs.tokens.is_some());
    let asking = Asking::Call(call.command, call.arguments);
    let answered = ask(
        (root, vault),
        reach,
        (&kept, &address),
        calling.by(),
        &asking,
    )
    .await;
    let ended = match &answered {
        Ok(Answer::Called(answered)) if answered.is_error => (Ended::Failed, Some(SAID_IT_FAILED)),
        Ok(_) => (Ended::Ok, None),
        Err(Problem::Said) => (Ended::Failed, Some(CALL_NOT_TAKEN)),
        Err(Problem::SignIn { .. }) if signed_in => {
            (Ended::Failed, Some(Why::SignInRanOut.words()))
        }
        Err(problem) => (Ended::Failed, Some(why(problem).words())),
    };
    kept::end_use(root, id, nth, ended.0, ended.1).map_err(|error| unavailable(&error))?;
    // How the service answered says how the connection stands, where it
    // says anything of the service and not of this one call. A sign-in
    // that ran out has said so already.
    let stands = match &answered {
        Ok(_) => Some(None),
        Err(problem @ (Problem::NoAnswer | Problem::SentOn)) => Some(Some(why(problem))),
        Err(_) => None,
    };
    let reached = answered.is_ok();
    kept::update(root, id, |kept| {
        if reached {
            kept.last_used_ms = Some(now);
        }
        if let Some(stands) = stands {
            kept.stand(stands, now);
        }
    })
    .map_err(|error| unavailable(&error))?;
    match answered {
        Ok(Answer::Called(answered)) => Ok(Called {
            content: answered.content,
            structured: answered.structured,
            is_error: answered.is_error,
        }),
        _ => Err(ended.1.unwrap_or(CALL_NOT_TAKEN).to_owned()),
    }
}

/// Begin signing in to the connection `id` names under `root`: where the
/// person's browser goes, and until when the sign-in can be finished.
/// `return_to` is where the browser comes back to, an address the person's
/// own app catches.
pub(crate) async fn begin_sign_in(
    root: &Path,
    reach: &Reach,
    id: &str,
    return_to: &str,
) -> Result<SignInStep, String> {
    let kept = kept_one(root, id)?;
    let address = Address::parse(&kept.address, reach.destinations)
        .map_err(|_| Why::NotCalled.words().to_owned())?;
    paced(root)?;
    let signs = match kept.signs_in {
        Some(signs) => signs,
        // The service did not say how when it was added: ask it now.
        None => match once(reach, &address, None, &Asking::Commands).await {
            Ok(_) => return Err("That service asks for no sign-in".into()),
            Err(Problem::SignIn { challenge }) => SignsIn {
                way: sign_in::discover(reach, &address, challenge.as_deref())
                    .await
                    .map_err(|_| sign_in::NO_WAY.to_owned())?,
                client: None,
                tokens: None,
            },
            Err(problem) => return Err(why(&problem).words().to_owned()),
        },
    };
    let (open, expires_at_ms, client) = sign_in::begin(
        (root, id),
        reach,
        &address,
        &signs.way,
        signs.client.as_ref(),
        return_to,
    )
    .await?;
    kept::update(root, id, |kept| {
        kept.sign_in_at = signs.way.site();
        kept.signs_in = Some(SignsIn {
            way: signs.way,
            client: Some(client),
            tokens: signs.tokens,
        });
    })
    .map_err(|error| unavailable(&error))?
    .ok_or_else(|| NOT_KEPT.to_owned())?;
    Ok(SignInStep {
        open,
        expires_at_ms,
    })
}

/// Finish signing in to the connection `id` names under `root`, with the
/// whole address the person's browser came back to: what the service's
/// issuer hands out goes into the person's vault, and the connection reads
/// what the service offers them.
pub(crate) async fn finish_sign_in(
    (root, vault): (&Path, VaultAt<'_>),
    reach: &Reach,
    id: &str,
    returned: &str,
) -> Result<Connection, String> {
    let kept = kept_one(root, id)?;
    let address = Address::parse(&kept.address, reach.destinations)
        .map_err(|_| Why::NotCalled.words().to_owned())?;
    let signs = kept.signs_in.ok_or(sign_in::BEGIN_AGAIN)?;
    let client = signs.client.as_ref().ok_or(sign_in::BEGIN_AGAIN)?;
    paced(root)?;
    let tokens = sign_in::finish(
        (root, id),
        vault,
        reach,
        &address,
        (&signs.way, client),
        returned,
    )
    .await?;
    // What it signed in with before is over.
    if let Some(before) = &signs.tokens {
        sign_in::forget(vault, before).await;
    }
    let keeps = tokens.clone();
    let signed = kept::update(root, id, |kept| {
        if let Some(signs) = &mut kept.signs_in {
            signs.tokens = Some(keeps);
        }
        kept.sign_in = SignIn::SignedIn;
    })
    .map_err(|error| unavailable(&error))?;
    if signed.is_none() {
        // A connection removed while its person signed in keeps no sign-in.
        sign_in::forget(vault, &tokens).await;
        return Err(NOT_KEPT.to_owned());
    }
    // The person is here: what the service lists now is what they saw.
    refresh((root, vault), reach, id, Calling::Person).await
}
