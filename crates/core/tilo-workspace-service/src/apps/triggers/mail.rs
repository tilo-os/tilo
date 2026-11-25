//! The triggers on mail that arrives in the workspace's accounts
//! ([`source_change::PERSON_MAIL`]): each message that arrived since the last look fires once.
//!
//! The messages read here are the Mail app's: its `Message` records, kept by the mail connector
//! as it brings them in. The trigger reads them as it reads any app's records, and nothing of a
//! mailbox is kept by the host that runs the shortcut. A message is new when its record was not
//! read before and it arrived after the trigger was first seen; one the connector rereads with a
//! sync is not new again. What a firing holds is the message's envelope and the start of its
//! words, which the trigger's filter compares and its run is told as data; nothing more of it is
//! copied anywhere. It names the message itself as what the run is made from (`$from`): the host
//! runs each step made from it only for a caller who may read it, and the whole message is read
//! in Mail by whoever may.
//!
//! What each trigger took in is kept in the workspace ([`super::keep_progress`]): a restart
//! fires nothing twice, and what arrived while nothing looked fires once.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::params::FROM_ARGUMENT;
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::{Event, SourceEvent};
use tilo_automation::source_change::{self, Found};

use super::{Runner, dispatched, fields_of, keep_progress, kept_progress};

/// The kind of record the Mail app keeps a message as.
const MESSAGE: &str = "tl.so.tilo.mail.Message";
/// The app whose records the messages are.
const APP: &str = "so.tilo.mail";
/// The most inbox messages one look reads.
const MOST_MESSAGES: usize = 5000;
/// The most messages a trigger keeps taken in: past it, the oldest are forgotten, and one still
/// in its inbox that old fires again rather than a message go unheard.
const MAX_HEARD: usize = source_change::MAX_KEYS;

/// What one trigger on mail took in, and whether it looked at all yet.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Heard {
    /// The trigger this is the progress of: another in its place starts over.
    #[serde(default)]
    pub mark: String,
    /// When the trigger was first seen, ms since the epoch: what arrived before is never new.
    #[serde(default)]
    pub since_ms: i64,
    /// Whether it looked once already: the first look takes in what is there and fires nothing.
    #[serde(default)]
    pub looked: bool,
    /// Each message taken in, by its key, with when it arrived, oldest first.
    #[serde(default)]
    pub heard: BTreeMap<String, i64>,
}

/// One message as its record holds it, read once.
struct Message {
    id: String,
    fields: Map<String, Value>,
}

impl Message {
    /// What names it for good: its account, folder and server key, or the record's id.
    fn key(&self) -> String {
        let text = |name: &str| {
            self.fields
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let uid = text("key");
        if uid.is_empty() {
            return self.id.clone();
        }
        format!("{}/{}/{uid}", text("account"), text("mailbox"))
    }

    /// When it arrived, ms since the epoch; none for a record whose time does not read.
    fn arrived_ms(&self) -> Option<i64> {
        let text = self.fields.get("received")?.as_str()?;
        let instant = tilo_failure::ignored!(
            "a message's time is read as an instant, or as a wall time in UTC",
            text.parse::<jiff::Timestamp>().ok()
        );
        instant.map(jiff::Timestamp::as_millisecond).or_else(|| {
            let performed = tilo_failure::ignored!(
                "a message's time that names no offset is read as UTC's wall time",
                text.parse::<jiff::civil::DateTime>().ok()
            )?;
            performed
                .to_zoned(jiff::tz::TimeZone::UTC)
                .ok()
                .map(|at| at.timestamp().as_millisecond())
        })
    }

    /// Whether it is mail that arrived, rather than one the person sent or kept.
    fn is_arrival(&self) -> bool {
        match self.fields.get("mailbox").and_then(Value::as_str) {
            Some(mailbox) => mailbox.is_empty() || mailbox.eq_ignore_ascii_case("inbox"),
            None => true,
        }
    }

    /// The message's own fields, as a trigger's filter reads them and its run is told them.
    fn found(&self) -> Found {
        let text = |name: &str| {
            self.fields
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let mut facts = Map::new();
        for (name, fact) in [
            ("from", text("from")),
            ("fromName", text("from_name")),
            ("to", text("to")),
            ("cc", text("cc")),
            ("subject", text("subject")),
            ("account", text("account")),
            ("folder", text("mailbox")),
            ("uid", text("key")),
            ("excerpt", text("snippet")),
            ("messageId", text("message_id")),
        ] {
            facts.insert(name.to_owned(), Value::String(fact));
        }
        if let Some(at) = self.arrived_ms() {
            facts.insert("atMs".to_owned(), Value::from(at));
        }
        facts.insert("id".to_owned(), Value::String(self.id.clone()));
        Found {
            key: self.key(),
            at: text("account"),
            at_ms: self.arrived_ms().unwrap_or_default(),
            facts,
        }
    }

    /// What the run made from it is made from: the message itself, as Mail holds it.
    fn made_from(&self) -> tilo_actions::params::MadeFrom {
        tilo_actions::params::MadeFrom {
            resource: format!("tl:{APP}:{}", self.id),
            kind: MESSAGE.to_owned(),
        }
    }
}

impl Runner {
    /// Fire the triggers on mail arriving in the workspace's accounts, one firing for each
    /// message that arrived since the trigger last looked.
    pub(super) async fn mail(&mut self, now_ms: i64) {
        let mut firings = Vec::new();
        for armed in &self.armed {
            for (index, trigger) in armed.shortcut.triggers.iter().enumerate() {
                let TriggerSpec::SourceChanged {
                    kind, at, filter, ..
                } = trigger
                else {
                    continue;
                };
                if kind != source_change::PERSON_MAIL {
                    continue;
                }
                let key = (armed.workspace.clone(), armed.shortcut.id.clone(), index);
                let mark = tilo_peer_state::trigger_mark(trigger);
                if self.heard.get(&key).is_none_or(|heard| heard.mark != mark) {
                    let kept = kept_progress::<Heard>(&self.workspaces, armed, index, &mark)
                        .await
                        .filter(|heard| heard.mark == mark);
                    self.heard.insert(
                        key.clone(),
                        kept.unwrap_or_else(|| Heard {
                            mark: mark.clone(),
                            since_ms: now_ms,
                            ..Heard::default()
                        }),
                    );
                }
                let messages = messages_in(&self.workspaces, &armed.workspace).await;
                let Some(heard) = self.heard.get_mut(&key) else {
                    continue;
                };
                let before = heard.clone();
                let Ok(messages) = messages else {
                    // Read again at the next look, whether or not anything was written.
                    continue;
                };
                let (since_ms, looked) = (heard.since_ms, heard.looked);
                let new = take_in(messages, &mut heard.heard, since_ms, looked);
                heard.looked = true;
                if *heard != before {
                    let kept = heard.clone();
                    keep_progress(&self.workspaces, armed, index, &mark, &kept).await;
                }
                for message in new {
                    let found = message.found();
                    if !(at.is_empty() || found.at.eq_ignore_ascii_case(at)) {
                        continue;
                    }
                    if !tilo_automation::resource_time::matches(filter, &found.facts) {
                        continue;
                    }
                    let mut payload = source_change::told(kind, &found);
                    payload.insert(FROM_ARGUMENT.to_owned(), message.made_from().to_json());
                    let event = SourceEvent {
                        workspace: armed.workspace.clone(),
                        trigger: index,
                        key: found.key,
                        payload,
                    };
                    firings.extend(dispatched(
                        std::slice::from_ref(armed),
                        Event::Source(&event),
                        now_ms,
                    ));
                }
            }
        }
        self.offer(firings);
    }
}

/// The messages `workspace` holds, as the Mail app keeps them.
async fn messages_in(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
) -> Result<Vec<Message>, String> {
    let found = workspaces
        .store_found(
            workspace,
            collab_types::store::Query::of(MESSAGE, MOST_MESSAGES),
        )
        .await?;
    Ok(found
        .into_iter()
        .map(|(id, held)| Message {
            id,
            fields: fields_of(&held),
        })
        .collect())
}

/// The mail that arrived in `workspace` since `heard` last took it in, each put into it: how
/// many messages are new. The first look (`looked` false) notes where the mail stands and
/// counts nothing, and a message a later sync rereads is not new again.
pub(crate) async fn arrived(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
    heard: &mut BTreeMap<String, i64>,
    looked: bool,
) -> Result<usize, String> {
    let messages = messages_in(workspaces, workspace).await?;
    Ok(take_in(messages, heard, i64::MIN, looked).len())
}

/// Take `messages` into `heard`, each by its key with when it arrived, and answer the ones that
/// are new since it last took them in: none on the first look, which notes where the mail
/// stands. A message that arrived before `since_ms` is not taken in at all, and past
/// [`MAX_HEARD`] the oldest are forgotten, so one still in its inbox that old fires again rather
/// than a message go unheard.
fn take_in(
    messages: Vec<Message>,
    heard: &mut BTreeMap<String, i64>,
    since_ms: i64,
    looked: bool,
) -> Vec<Message> {
    let mut new = Vec::new();
    for message in messages {
        if !message.is_arrival() {
            continue;
        }
        let Some(arrived) = message.arrived_ms() else {
            continue;
        };
        if arrived < since_ms {
            continue;
        }
        let named = message.key();
        if heard.contains_key(&named) {
            continue;
        }
        heard.insert(named, arrived);
        if looked {
            new.push(message);
        }
    }
    while heard.len() > MAX_HEARD {
        let Some(oldest) = heard
            .iter()
            .min_by_key(|(_, arrived)| **arrived)
            .map(|(named, _)| named.clone())
        else {
            break;
        };
        heard.remove(&oldest);
    }
    new
}
