//! Reads of existing records, answered by confined app workers.

use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_actions::remote::ViewResourceQuery;
use ui::record::RecordId;

use super::process::Process;
use super::proto::{Mode, Reply, Request};
use crate::room_actions::ViewReadPolicy;

fn json<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

fn descriptors(reply: Reply) -> Result<Vec<ActionDescriptor>, String> {
    match reply {
        Reply::Descriptors(text) => serde_json::from_str(&text).map_err(|error| error.to_string()),
        Reply::Failed(reason) => Err(reason),
        _ => Err("the app worker answered out of turn".into()),
    }
}

fn done(reply: Reply) -> Result<(), String> {
    match reply {
        Reply::Done => Ok(()),
        Reply::Failed(reason) => Err(reason),
        _ => Err("the app worker answered out of turn".into()),
    }
}

/// One record's component mounted headless in a confined app worker over
/// the host's retained source, for its registered reads. The host keeps the
/// policy checks; the worker is given the source and the caller's current
/// policy and runs the component.
pub(crate) struct ConfinedRead {
    process: Process,
}

impl ConfinedRead {
    /// Mount `record` of `app_id` in `room` over `source` under `policy`.
    ///
    /// # Errors
    ///
    /// The worker did not start or confine itself, or the component did not
    /// mount.
    pub(crate) fn prepare(
        app_id: &str,
        room: &str,
        record: &RecordId,
        source: Vec<u8>,
        policy: &ViewReadPolicy,
    ) -> Result<Self, String> {
        let mut process = Process::start(Mode::Read)?;
        done(process.call(&Request::ReadPrepare {
            app_id: app_id.to_owned(),
            room: room.to_owned(),
            record: record.as_str().to_owned(),
            source,
            policy: json(policy)?,
        })?)?;
        Ok(Self { process })
    }

    /// Replace the component's permissions with `policy`.
    ///
    /// # Errors
    ///
    /// The worker could not be asked.
    pub(crate) fn set_policy(&mut self, policy: &ViewReadPolicy) -> Result<(), String> {
        done(self.process.call(&Request::ReadPolicy {
            policy: json(policy)?,
        })?)
    }

    /// The registered reads the component offers now.
    ///
    /// # Errors
    ///
    /// The worker could not be asked.
    pub(crate) fn operations(&mut self) -> Result<Vec<ActionDescriptor>, String> {
        descriptors(self.process.call(&Request::ReadOperations)?)
    }

    /// Perform one registered read.
    ///
    /// # Errors
    ///
    /// The door's refusal, or the worker could not run it.
    pub(crate) fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, DoorError> {
        let refused = || DoorError::Unknown(address.clone());
        let request = Request::ReadInvoke {
            address: json(address).map_err(|_| refused())?,
            arguments: json(arguments).map_err(|_| refused())?,
        };
        match self.process.call(&request).map_err(|_| refused())? {
            Reply::Output(text) => serde_json::from_str(&text).map_err(|_| refused()),
            Reply::Refused(error) => Err(error.into_door(address)),
            _ => Err(refused()),
        }
    }
}

/// One existing document of one app loaded in a confined app worker, to
/// discover the views a resource of it offers.
pub(crate) struct ConfinedViews {
    process: Process,
}

impl ConfinedViews {
    /// Load `doc`, a document of `app_id`.
    ///
    /// # Errors
    ///
    /// The worker did not start or confine itself, or the bytes are not a
    /// document.
    pub(crate) fn load(app_id: &str, doc: Vec<u8>) -> Result<Self, String> {
        let mut process = Process::start(Mode::Views)?;
        done(process.call(&Request::ViewsLoad {
            app_id: app_id.to_owned(),
            doc,
        })?)?;
        Ok(Self { process })
    }

    /// The views `wanted` offers under `policy`: [`crate::AppCatalogue::resource_views`].
    ///
    /// # Errors
    ///
    /// The worker could not be asked.
    pub(crate) fn views(
        &mut self,
        wanted: &ViewResourceQuery,
        policy: &ViewReadPolicy,
    ) -> Result<Vec<ActionDescriptor>, String> {
        descriptors(self.process.call(&Request::Views {
            wanted: json(wanted)?,
            policy: json(policy)?,
        })?)
    }
}

/// What existing documents are called, read in one confined app worker
/// started on the first question and kept for the next.
#[derive(Default)]
pub(crate) struct ConfinedTitles {
    process: Option<Process>,
}

impl ConfinedTitles {
    /// What `doc`, a document of `app_id`, is called:
    /// [`crate::AppCatalogue::resource_title`].
    ///
    /// # Errors
    ///
    /// The worker did not start or confine itself, or could not be asked.
    pub(crate) fn title(&mut self, app_id: &str, doc: Vec<u8>) -> Result<Option<String>, String> {
        let process = match &mut self.process {
            Some(process) => process,
            None => self.process.insert(Process::start(Mode::Views)?),
        };
        let reply = process.call(&Request::Title {
            app_id: app_id.to_owned(),
            doc,
        });
        match reply {
            Ok(Reply::Title(title)) => Ok(title),
            Ok(Reply::Failed(reason)) => Err(reason),
            Ok(_) => {
                self.process = None;
                Err("the app worker answered out of turn".into())
            }
            Err(reason) => {
                // A worker that stopped answering is started again next time.
                self.process = None;
                Err(reason)
            }
        }
    }
}
