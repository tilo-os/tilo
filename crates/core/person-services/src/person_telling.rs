//! What may interrupt a person and what they were told, kept where their
//! workspace runs.
//!
//! A person says in their own words what is important to them, with the
//! situations those words depend on ([`person_telling::Importance`]), and
//! everything their assistant tells them joins one log, each thing marked
//! by that rule ([`told`], [`judge`]). The server keeps both in the person's
//! own store, beside their models and their arriving mail, and not in a
//! document of theirs a workspace carries: a person's own document is sealed
//! for their devices, and it is the server that reads the rule, judges and
//! tells while every device of theirs is off.
//!
//! It is the person's alone. The host answers the person on a device of
//! theirs, and an agent acting for them over such a device, which is what
//! the caller of a turn this host runs for them is. The agent reads the
//! rule and tells its person wherever it works for them; it changes the
//! rule, when its person tells it to, only in a workspace that person
//! owns. The log is read and acknowledged by the person alone. What a device does with a
//! notification (whether a kind breaks do-not-disturb, whether a
//! workspace's words leave the host in a push) only the person says.
//! Everybody else is told what a caller asking for something that does not
//! exist is told.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;
use tilo_actions::remote::person_telling::{
    self, Acknowledge, Fact, ForgetFact, Importance, Insist, Insisting, KeepOnHost, Kind,
    LetThrough, Line, ListAfter, MAX_FACT_NAME_BYTES, MAX_FACT_VALUE_BYTES, MAX_FACTS,
    MAX_LINE_BYTES, MAX_LINES, Offered, Say, SetFact, Tell, Teller, Unsay,
};
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

use crate::person_settings::{answer, argument};

pub mod addresses;
mod asks;
mod judge;
mod told;

pub use told::{Spenders, ToTell};

/// The longest id of a workspace a person names, in bytes.
const MAX_WORKSPACE_BYTES: usize = 128;

/// What this host holds for its people's tellings beside their stores.
pub(super) struct Telling {
    /// One writer at a time of anybody's rule or log.
    writing: Mutex<()>,
    /// Moves whenever anybody's log changed.
    told: tokio::sync::watch::Sender<u64>,
    /// What knows whose day a judgement is counted on.
    spenders: OnceLock<Arc<dyn Spenders>>,
    /// The people whose logs changed since whoever tells outside Tilo
    /// last asked.
    changed: Mutex<HashSet<[u8; 32]>>,
    /// Until when a device of each person is known to look for what they
    /// were told.
    looks: Mutex<HashMap<[u8; 32], told::Look>>,
    /// Whether something tells this host's people outside Tilo already.
    told_outside: std::sync::atomic::AtomicBool,
    /// Whose each address a push is sent to is, by its token: read from
    /// the stores the first time it is asked for.
    reached: Mutex<Option<HashMap<String, [u8; 32]>>>,
}

impl Default for Telling {
    fn default() -> Self {
        Self {
            writing: Mutex::default(),
            told: tokio::sync::watch::channel(0).0,
            spenders: OnceLock::new(),
            changed: Mutex::default(),
            looks: Mutex::default(),
            told_outside: std::sync::atomic::AtomicBool::new(false),
            reached: Mutex::default(),
        }
    }
}

/// `text`, cut at a character to at most `most` bytes where it is longer,
/// and then ending in an ellipsis.
fn clipped(text: &str, most: usize) -> String {
    if text.len() <= most {
        return text.to_owned();
    }
    let mut end = most.saturating_sub('…'.len_utf8());
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", text.get(..end).unwrap_or_default().trim_end())
}

/// Whoever asks, as the transport certified them and where they ask: what
/// a button they offer is held against.
type Asked<'a> = (
    Option<collab_types::WorkspaceId>,
    &'a Caller,
    Option<&'a DeviceCert>,
);

/// Who asks about a person's rule and log.
enum Asking {
    /// The person, on a device of theirs.
    Person([u8; 32]),
    /// An agent acting for the person over a device of theirs.
    Agent {
        person: [u8; 32],
        /// The id it acts under in the workspace it asks through.
        id: String,
        /// The name its person knows it under.
        name: String,
        /// The workspace it asks through, by its id spelled out.
        workspace: String,
        /// Whether its person owns that workspace.
        own: bool,
    },
}

impl Asking {
    const fn person(&self) -> &[u8; 32] {
        match self {
            Self::Person(person) | Self::Agent { person, .. } => person,
        }
    }

    /// The name a change is written down under: none for the person.
    fn by(&self) -> &str {
        match self {
            Self::Person(_) => "",
            Self::Agent { name, .. } => name,
        }
    }

    /// Whether `action` of the rule is this asker's to take. The person
    /// takes each. Their agent reads wherever it works for them, changes
    /// the rule only in a workspace of theirs, and never says what a
    /// device does with a notification.
    fn may(&self, action: &str) -> bool {
        match self {
            Self::Person(_) => true,
            Self::Agent { own, .. } => match action {
                person_telling::READ => true,
                person_telling::LET_THROUGH | person_telling::KEEP_ON_HOST => false,
                _ => *own,
            },
        }
    }
}

/// One change to a person's rule.
enum Change {
    Say(Say),
    Unsay(Unsay),
    SetFact(SetFact),
    ForgetFact(ForgetFact),
    SetInsisting(Insisting),
    LetThrough(LetThrough),
    KeepOnHost(KeepOnHost),
}

impl Change {
    /// The change `request` asks for.
    fn of(request: &InvokeRequest) -> Result<Self, ActionCallError> {
        Ok(match request.target.address.action.as_str() {
            person_telling::SAY => Self::Say(argument(request, "a line and its words")?),
            person_telling::UNSAY => Self::Unsay(argument(request, "a line by its id")?),
            person_telling::SET_FACT => {
                Self::SetFact(argument(request, "a situation and how it stands")?)
            }
            person_telling::FORGET_FACT => {
                Self::ForgetFact(argument(request, "a situation by its name")?)
            }
            person_telling::SET_INSISTING => {
                Self::SetInsisting(argument(request, "how often and how many times")?)
            }
            person_telling::LET_THROUGH => {
                Self::LetThrough(argument(request, "a kind and whether it may")?)
            }
            person_telling::KEEP_ON_HOST => {
                Self::KeepOnHost(argument(request, "a workspace and whether it is kept")?)
            }
            _ => return Err(ActionCallError::UnknownAction),
        })
    }

    /// Make this change to `rule`, written down as `by`'s at `now_ms`.
    fn apply(self, rule: &mut Importance, by: &str, now_ms: u64) -> Result<(), String> {
        match self {
            Self::Say(saying) => say(rule, saying, by, now_ms),
            Self::Unsay(unsaying) => {
                let before = rule.lines.len();
                rule.lines.retain(|line| line.id != unsaying.line);
                if rule.lines.len() == before {
                    return Err("No line has that id".to_owned());
                }
                Ok(())
            }
            Self::SetFact(setting) => set_fact(rule, &setting, by, now_ms),
            Self::ForgetFact(forgetting) => forget_fact(rule, &forgetting.name),
            Self::SetInsisting(insisting) => {
                rule.insists = insisting.checked()?;
                Ok(())
            }
            Self::LetThrough(letting) => {
                rule.through_quiet.retain(|kind| *kind != letting.kind);
                if letting.may {
                    rule.through_quiet.push(letting.kind);
                }
                Ok(())
            }
            Self::KeepOnHost(keeping) => {
                let workspace = keeping.workspace.trim();
                if workspace.is_empty() || workspace.len() > MAX_WORKSPACE_BYTES {
                    return Err("A workspace is named by its id".to_owned());
                }
                rule.kept_on_host.retain(|kept| kept != workspace);
                if keeping.kept {
                    rule.kept_on_host.push(workspace.to_owned());
                }
                Ok(())
            }
        }
    }
}

/// Whether two names a person wrote are the same.
fn same(left: &str, right: &str) -> bool {
    left.trim().to_lowercase() == right.trim().to_lowercase()
}

/// Add `saying` to `rule` as one line. The same words said again are the
/// same line, with what it waits on as said now.
fn say(rule: &mut Importance, saying: Say, by: &str, now_ms: u64) -> Result<(), String> {
    let words = saying.words.trim();
    if words.is_empty() {
        return Err("A line needs words".to_owned());
    }
    if words.len() > MAX_LINE_BYTES {
        return Err("That is longer than a line may be".to_owned());
    }
    let only_while = match saying.only_while {
        None => None,
        Some(holds) => {
            let (fact, is) = (holds.fact.trim(), holds.is.trim());
            if fact.is_empty()
                || is.is_empty()
                || fact.len() > MAX_FACT_NAME_BYTES
                || is.len() > MAX_FACT_VALUE_BYTES
            {
                return Err(
                    "A line that depends on a situation names it and how it stands".to_owned(),
                );
            }
            Some(person_telling::Holds {
                fact: fact.to_owned(),
                is: is.to_owned(),
            })
        }
    };
    if let Some(line) = rule.lines.iter_mut().find(|line| same(&line.words, words)) {
        line.only_while = only_while;
        by.clone_into(&mut line.by);
        line.at_ms = now_ms;
        return Ok(());
    }
    if rule.lines.len() >= MAX_LINES {
        return Err("The rule holds no more lines".to_owned());
    }
    rule.lines.push(Line {
        id: format!("line-{}", rule.revision.saturating_add(1)),
        words: words.to_owned(),
        only_while,
        by: by.to_owned(),
        at_ms: now_ms,
    });
    Ok(())
}

/// Set the situation `setting` names, adding it when the rule has none of
/// that name.
fn set_fact(rule: &mut Importance, setting: &SetFact, by: &str, now_ms: u64) -> Result<(), String> {
    let (name, value) = (setting.name.trim(), setting.value.trim());
    if name.is_empty() || value.is_empty() {
        return Err("A situation has a name and says how it stands".to_owned());
    }
    if name.len() > MAX_FACT_NAME_BYTES || value.len() > MAX_FACT_VALUE_BYTES {
        return Err("That is longer than a situation may be".to_owned());
    }
    if let Some(fact) = rule.facts.iter_mut().find(|fact| same(&fact.name, name)) {
        value.clone_into(&mut fact.value);
        by.clone_into(&mut fact.by);
        fact.at_ms = now_ms;
        return Ok(());
    }
    if rule.facts.len() >= MAX_FACTS {
        return Err("The rule holds no more situations".to_owned());
    }
    rule.facts.push(Fact {
        name: name.to_owned(),
        value: value.to_owned(),
        by: by.to_owned(),
        at_ms: now_ms,
    });
    Ok(())
}

/// Remove the situation called `name`, unless a line still waits on it.
fn forget_fact(rule: &mut Importance, name: &str) -> Result<(), String> {
    if rule.fact(name).is_none() {
        return Err("No situation has that name".to_owned());
    }
    let waited_on = rule.lines.iter().any(|line| {
        line.only_while
            .as_ref()
            .is_some_and(|holds| same(&holds.fact, name))
    });
    if waited_on {
        return Err("A line still depends on that situation".to_owned());
    }
    rule.facts.retain(|fact| !same(&fact.name, name));
    Ok(())
}

fn rule_at(root: &Path) -> PathBuf {
    root.join("telling").join("importance.json")
}

/// The rule kept under `root`: nothing said where none is kept. One that
/// does not read is nothing said, and says so in the host's log: nothing
/// is important by a rule nobody can read.
fn read_rule(root: &Path) -> Importance {
    let bytes = match std::fs::read(rule_at(root)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Importance::default(),
        Err(error) => {
            tracing::warn!(%error, "telling: a person's rule could not be read");
            return Importance::default();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        tracing::warn!(%error, "telling: a person's rule does not read");
        Importance::default()
    })
}

/// Keep `rule` under `root`, for its owner alone to read.
fn write_rule(root: &Path, rule: &Importance) -> Result<(), String> {
    let bytes = serde_json::to_vec(rule).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(&rule_at(root), &bytes, 0o600)
        .map_err(|error| error.to_string())
}

/// What `rule` is told to its person's assistant before every turn: the
/// lines as the person said them, how the situations stand, and how a
/// change the person asks for is written down.
fn told_in_a_turn(rule: &Importance) -> String {
    let mut told = String::new();
    if rule.lines.is_empty() {
        told.push_str("Your person has not said what is important to them.");
    } else {
        told.push_str("What your person calls important, in their own words:");
        for line in &rule.lines {
            let _ = write!(told, "\n- {}", line.words);
            if let Some(holds) = &line.only_while {
                let _ = write!(
                    told,
                    " (counts only while `{}` is `{}`)",
                    holds.fact, holds.is
                );
            }
        }
    }
    if !rule.facts.is_empty() {
        told.push_str("\nHow the situations stand now:");
        for fact in &rule.facts {
            let _ = write!(told, "\n- {}: {}", fact.name, fact.value);
        }
    }
    told.push_str(
        "\nWhen they tell you that something is or is not important to them, or that a \
         situation changed, write it down with the actions of `tilo.telling` on `importance` \
         (`say` with their own words, and `onlyWhile` when the line depends on a situation; \
         `unsay`; `set-fact`; `forget-fact`). You never decide what is important and you never \
         notify: what you write for them is marked by their rule and told for you.",
    );
    told
}

/// The arguments of a request that may name none.
fn argument_or_default<T: serde::de::DeserializeOwned + Default>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionCallError> {
    if matches!(request.argument, InvokeArgument::None) {
        return Ok(T::default());
    }
    argument(request, what)
}

impl crate::PersonServices {
    /// Who `caller` is to a person's rule and log: the person on a device
    /// of theirs, or an agent acting for them over one, as the transport
    /// certified. Nobody else is anybody here.
    fn asking_about_telling(
        &self,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> Result<Asking, ActionCallError> {
        if let Ok(person) = tilo_workspace_service::workspaces::requests::asking_person(
            caller,
            certificate,
            workspace_id,
        ) {
            return Ok(Asking::Person(person));
        }
        let workspace = workspace_id
            .ok_or(ActionCallError::UnknownAction)?
            .to_string();
        let acting = caller.acting();
        let (Some(agent), Some(person)) = (acting.agent_id(), acting.acted_for_person()) else {
            return Err(ActionCallError::UnknownAction);
        };
        let person: [u8; 32] = hex::decode(person)
            .ok()
            .and_then(|key| key.try_into().ok())
            .ok_or(ActionCallError::UnknownAction)?;
        // An agent is a person's when a key of that person's stands behind
        // its connection, as the transport certified.
        let certified = certificate.is_some_and(|certificate| {
            certificate.user_public_key == person
                && caller.device_public_key() == Some(&certificate.device_public_key)
                && caller.user_public_key() == Some(&certificate.user_public_key)
        });
        if !certified {
            return Err(ActionCallError::UnknownAction);
        }
        let agent = agent.to_owned();
        Ok(Asking::Agent {
            own: self.owns(&person, &workspace),
            name: caller
                .agent_name()
                .map_or_else(|| agent.clone(), str::to_owned),
            id: agent,
            workspace,
            person,
        })
    }

    /// Whether `caller` is an agent of a person's that the rule's and the
    /// log's actions are offered to, and if so whether it asks through a
    /// workspace that person owns: what the catalogue offers them by.
    pub(super) fn tells_as_agent(
        &self,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> Option<bool> {
        match self.asking_about_telling(caller, certificate, workspace_id) {
            Ok(Asking::Agent { own, .. }) => Some(own),
            _ => None,
        }
    }

    /// Answer a [`person_telling::PROVIDER`] request.
    pub(super) async fn person_telling_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let asking = self.asking_about_telling(caller, certificate, workspace_id)?;
        match request.target.address.resource.as_str() {
            person_telling::IMPORTANCE => self.importance_invoke(&asking, request),
            person_telling::TOLD => {
                self.told_invoke(&asking, (workspace_id, caller, certificate), request)
                    .await
            }
            // Where a device is reached is handed over by that device, for
            // its person alone.
            addresses::RESOURCE => {
                let (Asking::Person(person), Some(certificate)) = (&asking, certificate) else {
                    return Err(ActionCallError::UnknownAction);
                };
                self.addresses_invoke(person, &certificate.device_public_key, request)
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Answer a request about a person's rule.
    fn importance_invoke(
        &self,
        asking: &Asking,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let action = request.target.address.action.as_str();
        // Unknown and forbidden are the same answer, given before the
        // request is read.
        if !asking.may(action) {
            return Err(ActionCallError::UnknownAction);
        }
        if action == person_telling::READ {
            return answer(&Ok::<_, String>(self.importance_of(asking.person())));
        }
        let change = Change::of(request)?;
        answer(&self.change_importance(asking.person(), change, asking.by()))
    }

    /// Answer a request about what a person was told: an agent of theirs
    /// tells them, and only the person reads what they were told.
    async fn told_invoke(
        &self,
        asking: &Asking,
        asked: Asked<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        match (request.target.address.action.as_str(), asking) {
            (
                person_telling::TELL,
                Asking::Agent {
                    person,
                    id,
                    name,
                    workspace,
                    ..
                },
            ) => {
                let tell: Tell = argument(request, "what to tell")?;
                if let Err(refused) = self.offers_its_own(asked, &tell.actions).await {
                    return answer(&Err::<Value, String>(refused));
                }
                let teller = Teller {
                    agent: id.clone(),
                    name: name.clone(),
                };
                let offered_by = (!tell.actions.is_empty()).then(|| teller.clone());
                let telling = ToTell {
                    id: None,
                    // What an agent tells is news, whatever cause it
                    // names, and its person's rule marks it. What reaches a
                    // person unjudged, a reminder they set up or a brief
                    // they asked for, is told by what carries out that
                    // event of theirs, never by whoever says it is one.
                    kind: Kind::News,
                    tell,
                    from: Some(teller),
                    workspace: Some(workspace.clone()),
                    words_stay: false,
                    spender: None,
                };
                answer(&self.tell_offered(person, telling, offered_by).await)
            }
            (person_telling::LIST, Asking::Person(person)) => {
                let after: ListAfter = argument_or_default(request, "a place in the log")?;
                answer(&Ok::<_, String>(self.told_listed(person, &after).await))
            }
            (person_telling::ACKNOWLEDGE, Asking::Person(person)) => {
                let seen: Acknowledge = argument(request, "the thing by its id")?;
                answer(&self.acknowledge_told(person, &seen.told, &seen.device, &seen.by))
            }
            (person_telling::SINCE_YOU_LEFT, Asking::Person(person)) => {
                answer(&Ok::<_, String>(self.since_left_for_device(person)))
            }
            (person_telling::INSIST, asking) => {
                let insist: Insist = argument(request, "the thing by its id, and whether")?;
                let theirs = matches!(asking, Asking::Person(_));
                answer(&self.insist_told(asking.person(), &insist.told, insist.insists, theirs))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Refuse `actions`, the buttons an agent's own telling offers, unless
    /// each is an action this server offers that agent itself where it
    /// asks. A button is pressed by the person and invoked as the person,
    /// so one aimed at anything else would have the person do for the
    /// agent what it may not do alone. A workspace's governance and this
    /// server's own operations are never a button.
    async fn offers_its_own(
        &self,
        (workspace_id, caller, certificate): Asked<'_>,
        actions: &[Offered],
    ) -> Result<(), String> {
        // Bounded before anything is looked up for them.
        if actions.len() > person_telling::MAX_ACTIONS {
            return Err(format!(
                "A thing told offers at most {} buttons",
                person_telling::MAX_ACTIONS
            ));
        }
        for action in actions {
            let target = &action.target;
            let query = CatalogueQuery {
                provider: Some(target.app.clone()),
                max_results: 1000,
                ..CatalogueQuery::default()
            };
            let its_own = !tilo_workspace_service::workspaces::actions::governs(&target.app)
                && target.record.is_empty()
                && self
                    .catalogue(workspace_id, caller, certificate, &query)
                    .await
                    .rows
                    .iter()
                    .filter_map(|row| row.action.as_ref())
                    .any(|offered| {
                        offered.address.resource == target.resource
                            && offered.address.action == target.action
                    });
            if !its_own {
                return Err(
                    "A button invokes only an action its teller may invoke itself".to_owned(),
                );
            }
        }
        Ok(())
    }

    /// Make `change` to `person`'s rule and answer the rule as it stands
    /// after.
    fn change_importance(
        &self,
        person: &[u8; 32],
        change: Change,
        by: &str,
    ) -> Result<Importance, String> {
        let root = self.person_settings_root(person);
        let rule = {
            let _writing = self.inner.telling.writing.lock_recover();
            let mut rule = read_rule(&root);
            change.apply(&mut rule, by, tilo_clock::unix_now_ms())?;
            rule.revision = rule.revision.saturating_add(1);
            write_rule(&root, &rule)?;
            rule
        };
        Ok(rule)
    }

    /// What `person` calls important, as this host keeps it: nothing said
    /// for a person who said nothing.
    #[must_use]
    pub fn importance_of(&self, person: &[u8; 32]) -> Importance {
        let _writing = self.inner.telling.writing.lock_recover();
        read_rule(&self.person_settings_root(person))
    }

    /// What `person`'s assistant is told about their rule before every
    /// turn: the lines as the person said them, how the situations stand,
    /// and how a change the person asks for is written down.
    #[must_use]
    pub fn importance_told(&self, person: &[u8; 32]) -> String {
        told_in_a_turn(&self.importance_of(person))
    }

    /// Keep `rule` as `person`'s, whole, as their own changes would have
    /// left it: what a test of whatever reads the rule states it with.
    ///
    /// # Errors
    /// The rule could not be written.
    #[cfg(any(test, feature = "test-support"))]
    pub fn keep_importance(&self, person: &[u8; 32], rule: &Importance) -> Result<(), String> {
        let _writing = self.inner.telling.writing.lock_recover();
        write_rule(&self.person_settings_root(person), rule)
    }
}

#[cfg(test)]
mod tests {
    use tilo_actions::remote::person_telling::Holds;

    use super::*;

    /// The assistant is told the rule in the person's own words, how the
    /// situations stand, and how to write a change down; a person who said
    /// nothing is not told a rule they do not have.
    #[test]
    fn a_turn_is_told_the_rule_as_its_person_said_it() {
        let nothing = told_in_a_turn(&Importance::default());
        assert!(nothing.starts_with("Your person has not said what is important to them."));
        assert!(nothing.contains("`tilo.telling`"), "{nothing}");

        let rule = Importance {
            lines: vec![
                Line {
                    id: "line-1".into(),
                    words: "My landlord is always important".into(),
                    ..Line::default()
                },
                Line {
                    id: "line-2".into(),
                    words: "A new customer is important only while we are hiring".into(),
                    only_while: Some(Holds {
                        fact: "hiring".into(),
                        is: "yes".into(),
                    }),
                    ..Line::default()
                },
            ],
            facts: vec![Fact {
                name: "Hiring".into(),
                value: "no".into(),
                ..Fact::default()
            }],
            ..Importance::default()
        };
        let told = told_in_a_turn(&rule);
        assert!(
            told.contains("\n- My landlord is always important\n"),
            "{told}"
        );
        assert!(
            told.contains(
                "\n- A new customer is important only while we are hiring (counts only while \
                 `hiring` is `yes`)"
            ),
            "{told}"
        );
        assert!(told.contains("\n- Hiring: no"), "{told}");
        assert!(
            told.contains("You never decide what is important"),
            "{told}"
        );
    }

    /// Words longer than a notification shows are cut at a character and
    /// say that they were.
    #[test]
    fn long_words_are_cut_at_a_character() {
        assert_eq!(clipped("short", 10), "short");
        let cut = clipped("grüße an alle zusammen", 8);
        assert!(cut.len() <= 8, "{cut}");
        assert!(cut.ends_with('…'), "{cut}");
        assert!(cut.starts_with("grü"), "{cut}");
    }
}

#[cfg(test)]
#[path = "addresses_tests.rs"]
mod addresses_tests;
#[cfg(test)]
#[path = "telling_tests.rs"]
mod telling_tests;
#[cfg(test)]
#[path = "told_tests.rs"]
mod told_tests;
