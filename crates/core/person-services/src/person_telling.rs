//! What a person was told, kept where their workspace runs.
//!
//! Everything a person's assistant tells them outside an answer to their
//! own words joins one log in the person's own store on this host
//! ([`told`]), each thing's mark decided by what kind of thing it is. The
//! server keeps it beside their models and their arriving mail, and not in
//! a document of theirs a workspace carries: it is the server that tells
//! while every device of theirs is off.
//!
//! It is the person's alone. The host answers the person on a device of
//! theirs, and an agent acting for them over such a device, which is what
//! the caller of a turn this host runs for them is. The agent tells its
//! person wherever it works for them. The log is read and acknowledged by
//! the person alone. Everybody else is told what a caller asking for
//! something that does not exist is told.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
};
use tilo_resource::Effect as _;
use tilo_user_identity::DeviceCert;

use crate::person_settings::{answer, argument};

pub mod addresses;
mod asks;
pub mod declared;
pub mod records;
mod told;

pub use declared::{ToldAction, offered_to_agent, target};
pub use records::*;

pub use told::ToTell;

/// What this host holds for its people's tellings beside their stores.
pub(super) struct Telling {
    /// One writer at a time of anybody's log.
    writing: Mutex<()>,
    /// Moves whenever anybody's log changed.
    told: tokio::sync::watch::Sender<u64>,
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

/// Who asks about a person's log.
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
    },
}

impl Asking {
    const fn person(&self) -> &[u8; 32] {
        match self {
            Self::Person(person) | Self::Agent { person, .. } => person,
        }
    }
}

/// The call `request` makes, read as its action declares it.
fn asked_of(request: &InvokeRequest) -> Result<ToldAction, ActionCallError> {
    let arguments = match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        _ => serde_json::Map::new(),
    };
    ToldAction::decode(&request.target.address.action, &arguments).map_err(|error| match error {
        tilo_resource::EffectDecodeError::Unknown(_) => ActionCallError::UnknownAction,
        tilo_resource::EffectDecodeError::Argument { name, .. } => {
            ActionCallError::invalid_request(format!("The `{name}` argument is not readable"))
        }
        other => ActionCallError::invalid_request(other.to_string()),
    })
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
    /// Who `caller` is to a person's log: the person on a device of
    /// theirs, or an agent acting for them over one, as the transport
    /// certified. Nobody else is anybody here.
    fn asking_about_telling(
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
        if workspace_id.is_none() {
            return Err(ActionCallError::UnknownAction);
        }
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
            name: caller
                .agent_name()
                .map_or_else(|| agent.clone(), str::to_owned),
            id: agent,
            person,
        })
    }

    /// Whether `caller` is an agent of a person's that the log's actions
    /// are offered to: what the catalogue offers them by.
    pub(super) fn tells_as_agent(
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> bool {
        matches!(
            Self::asking_about_telling(caller, certificate, workspace_id),
            Ok(Asking::Agent { .. })
        )
    }

    /// Answer a [`PROVIDER`] request.
    pub(super) async fn person_telling_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let asking = Self::asking_about_telling(caller, certificate, workspace_id)?;
        match request.target.address.resource.as_str() {
            records::TOLD => {
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

    /// Answer a request about what a person was told: an agent of theirs
    /// tells them, and only the person reads what they were told.
    async fn told_invoke(
        &self,
        asking: &Asking,
        asked: Asked<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        match (asked_of(request)?, asking) {
            (
                ToldAction::Tell {
                    words,
                    title,
                    cause,
                    about,
                    task,
                    actions,
                    insists,
                },
                Asking::Agent { person, id, name },
            ) => {
                let actions = actions.unwrap_or_default();
                if let Err(refused) = self.offers_its_own(asked, &actions).await {
                    return answer(&Err::<Value, String>(refused));
                }
                let teller = Teller {
                    agent: id.clone(),
                    name: name.clone(),
                };
                let offered_by = (!actions.is_empty()).then(|| teller.clone());
                let telling = ToTell {
                    id: None,
                    // What an agent tells is news, whatever cause it
                    // names. What reaches a person by what it is, a
                    // reminder they set up or a brief they asked for, is
                    // told by what carries out that event of theirs, never
                    // by whoever says it is one.
                    kind: Kind::News,
                    words,
                    title: title.unwrap_or_default(),
                    cause,
                    about,
                    task,
                    actions,
                    insists: insists.unwrap_or(false),
                    from: Some(teller),
                    words_stay: false,
                };
                answer(&self.tell_offered(person, telling, offered_by))
            }
            (ToldAction::List { after, wait_ms, .. }, Asking::Person(person)) => {
                answer(&Ok::<_, String>(
                    self.told_listed(person, after.unwrap_or(0), wait_ms).await,
                ))
            }
            (ToldAction::Acknowledge { told, device, by }, Asking::Person(person)) => {
                answer(&self.acknowledge_told(
                    person,
                    &told,
                    &device.unwrap_or_default(),
                    &by.unwrap_or_default(),
                ))
            }
            (ToldAction::SinceYouLeft, Asking::Person(person)) => {
                answer(&Ok::<_, String>(self.since_left_for_device(person)))
            }
            (ToldAction::Insist { told, insists }, asking) => {
                let theirs = matches!(asking, Asking::Person(_));
                answer(&self.insist_told(asking.person(), &told, insists, theirs))
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
        if actions.len() > MAX_ACTIONS {
            return Err(format!("A thing told offers at most {MAX_ACTIONS} buttons"));
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
#[path = "told_tests.rs"]
mod told_tests;
