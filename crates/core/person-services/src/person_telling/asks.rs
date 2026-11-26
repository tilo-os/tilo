//! The calls waiting for a person's yes, told to them: each asked record written is a thing of
//! their log, which their devices raise as a notification and which reaches a browser of theirs
//! when no device is looking: a question with Approve and Decline, which answer the record
//! itself, or, where only they can type what it takes, none, and opening it shows the ask. Once
//! the record is answered anywhere, the thing waits no more.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_actions::remote::asked::{self, AskedBy, AskedFacts, AskedRecords, RecordRef};
use tilo_actions::remote::person_telling::{About, Kind, Offered, OfferedTarget, Tell, Teller};
use tilo_actions::remote::{Caller, InvokeRequest, Pending};
use tilo_lock::MutexRecover as _;

use crate::{PersonServices, ToTell, WeakPersonServices};

/// The person `caller` asks, as their key.
fn asked_person(caller: &Caller) -> Option<[u8; 32]> {
    hex::decode(asked::asked_person(caller)?)
        .ok()?
        .try_into()
        .ok()
}

/// The id the record `record` is told under in its person's log.
fn told_id(record: &RecordRef) -> String {
    format!("asked:{}/{}", record.workspace, record.record)
}

/// What the person reads of `facts` where the record is told: the asker's own words, else
/// the verb and what it is on.
fn told_words(facts: &AskedFacts) -> String {
    if !facts.words.trim().is_empty() {
        return facts.words.clone();
    }
    match &facts.on {
        Some(on) => format!("{} {on}", facts.verb),
        None => facts.verb.clone(),
    }
}

/// The thing the asked `record` is told as: who asks, in their words, with Approve and
/// Decline answering the record, except where only the person can type what it takes.
fn telling(record: &RecordRef, facts: &AskedFacts) -> ToTell {
    let answer = |id: &str, label: &str, main: bool| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("resolution".into(), Value::String(id.to_owned()));
        Offered {
            id: id.to_owned(),
            label: label.to_owned(),
            main,
            target: OfferedTarget {
                app: tilo_actions::remote::workspace_invocations::PROVIDER.to_owned(),
                record: String::new(),
                resource: record.record.clone(),
                action: asked::RESOLVE.to_owned(),
            },
            arguments,
        }
    };
    let actions = if facts.answered_by.is_some() || !facts.takes.is_empty() {
        Vec::new()
    } else {
        vec![
            answer("approve", "Approve", true),
            answer("decline", "Decline", false),
        ]
    };
    let from = match &facts.by {
        AskedBy::Agent(agent) => Some(Teller {
            agent: agent.id.clone(),
            name: agent.name.clone(),
        }),
        AskedBy::App { .. } | AskedBy::Device(_) | AskedBy::Program { .. } => None,
    };
    ToTell {
        id: Some(told_id(record)),
        kind: Kind::Question,
        tell: Tell {
            words: told_words(facts),
            about: Some(About {
                workspace: record.workspace.clone(),
                app: tilo_actions::remote::workspace_invocations::PROVIDER.to_owned(),
                record: String::new(),
                resource: record.record.clone(),
            }),
            actions,
            ..Tell::default()
        },
        from,
        words_stay: false,
    }
}

/// The asked records as the host keeps them: each one written is told to the person it waits
/// for, and taken back from their log once it is answered.
struct Told {
    records: Arc<dyn AskedRecords>,
    workspaces: WeakPersonServices,
    /// The records told and not yet seen answered, with the person each waits for.
    waiting: Mutex<BTreeSet<([u8; 32], RecordRef)>>,
}

impl Told {
    /// Take back from their log each record told that waits no more.
    fn settle(&self, workspaces: &PersonServices) {
        let waiting = self.waiting.lock_recover().clone();
        let mut asked: std::collections::BTreeMap<[u8; 32], BTreeSet<RecordRef>> =
            std::collections::BTreeMap::new();
        for (person, record) in waiting {
            let still = asked.entry(person).or_insert_with(|| {
                self.records
                    .asked_of(&person)
                    .into_iter()
                    .map(|asked| asked.record)
                    .collect()
            });
            if still.contains(&record) {
                continue;
            }
            if let Err(error) = workspaces.withdraw_told(&person, &told_id(&record)) {
                tracing::debug!(%error, "an answered ask stays in its person's log");
            }
            self.waiting.lock_recover().remove(&(person, record));
        }
    }
}

impl AskedRecords for Told {
    fn write_asked(
        &self,
        workspace: &str,
        caller: &Caller,
        request: &InvokeRequest,
        facts: &AskedFacts,
    ) -> Result<RecordRef, String> {
        let record = self
            .records
            .write_asked(workspace, caller, request, facts)?;
        let value = (asked_person(caller), self.workspaces.upgrade());
        if let (Some(person), Some(workspaces)) = value {
            self.waiting.lock_recover().insert((person, record.clone()));
            let told = telling(&record, facts);
            if let Err(error) = workspaces.tell(&person, told) {
                tracing::warn!(%error, "an asked record was not told to its person");
            }
        }
        Ok(record)
    }

    fn asked_of(&self, person: &[u8; 32]) -> Vec<asked::Asked> {
        self.records.asked_of(person)
    }

    fn changed(&self) -> Pending<()> {
        self.records.changed()
    }
}

impl PersonServices {
    /// `records`, each one written told to the person it waits for and taken back from their
    /// log once it is answered: what every dispatch of this host keeps its asked calls through.
    pub fn told(&self, records: Arc<dyn AskedRecords>) -> Arc<dyn AskedRecords> {
        let told = Arc::new(Told {
            records,
            workspaces: self.downgrade(),
            waiting: Mutex::default(),
        });
        let follows = Arc::downgrade(&told);
        tokio::spawn(async move {
            loop {
                let Some(changed) = follows.upgrade().map(|told| told.records.changed()) else {
                    return;
                };
                if changed.await.is_none() {
                    return;
                }
                let (Some(told), Some(workspaces)) = (follows.upgrade(), told_workspaces(&follows))
                else {
                    return;
                };
                told.settle(&workspaces);
            }
        });
        told
    }
}

/// The workspaces `told` tells for, while both are still there.
fn told_workspaces(told: &std::sync::Weak<Told>) -> Option<PersonServices> {
    told.upgrade()?.workspaces.upgrade()
}
