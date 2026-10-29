//! A service in a process of its own, reached over JSON lines on its
//! standard input and output: the session writes the actions invoked
//! ([`Request`]), the service writes its readings whenever one changes
//! ([`Event`]).
//!
//! Specs do not travel: both sides build them from the readings with the
//! service crate's own `publish` function, so the session checks an
//! invocation's arguments and availability against the same specs the
//! service does before it writes it.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::sync::mpsc::{self, Receiver, Sender};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};

use crate::{Service, Started, Wake};

/// What the session asks of the service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Request {
    /// Carry out an action.
    Invoke {
        resource: String,
        action: String,
        arguments: Map<String, Value>,
    },
}

/// What the service tells the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Event {
    /// A resource reads this now.
    Reading { resource: String, value: Value },
    /// The service refused an action, with why.
    Refused {
        resource: String,
        action: String,
        reason: String,
    },
}

impl Event {
    /// The line this event is written as.
    #[must_use]
    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Builds a service's specs from its readings, by resource id.
pub type Publish = fn(&BTreeMap<String, Value>) -> Vec<ResourceSpec>;

/// What the serving loop waits on.
pub enum Incoming {
    /// A line the session wrote.
    Line(String),
    /// The service's worker landed something.
    Woken,
    /// The session closed the service's input: stop.
    Closed,
}

/// Where the serving loop's [`Incoming`] come from, and the [`Wake`] to
/// hand the service's worker.
#[must_use]
pub fn incoming() -> (Sender<Incoming>, Receiver<Incoming>, Wake) {
    let (sender, receiver) = mpsc::channel();
    let woken = sender.clone();
    let wake: Wake = std::sync::Arc::new(move || {
        let _ = woken.send(Incoming::Woken);
    });
    (sender, receiver, wake)
}

/// Feed the lines of `input` to `sender` until it ends, then
/// [`Incoming::Closed`]; for a thread of its own.
pub fn read_lines(input: impl BufRead, sender: &Sender<Incoming>) {
    for line in input.lines() {
        let Ok(line) = line else { break };
        if sender.send(Incoming::Line(line)).is_err() {
            return;
        }
    }
    let _ = sender.send(Incoming::Closed);
}

/// Serve `service` until the input closes: write every reading at the
/// start and again whenever it changes, carry out each request.
///
/// # Errors
/// The output could not be written.
pub fn serve(
    service: &mut dyn Service,
    incoming: &Receiver<Incoming>,
    mut output: impl Write,
) -> std::io::Result<()> {
    let mut written: BTreeMap<String, Value> = BTreeMap::new();
    write_changed(service, &mut written, &mut output)?;
    for message in incoming {
        match message {
            Incoming::Closed => break,
            Incoming::Woken => {
                service.poll();
            }
            Incoming::Line(line) => match serde_json::from_str::<Request>(&line) {
                Ok(Request::Invoke {
                    resource,
                    action,
                    arguments,
                }) => {
                    if let Err(error) = service.invoke(&resource, &action, &arguments) {
                        let refused = Event::Refused {
                            resource,
                            action,
                            reason: error.to_string(),
                        };
                        writeln!(output, "{}", refused.line())?;
                    }
                }
                Err(error) => tracing::warn!(%error, "a request line did not read"),
            },
        }
        write_changed(service, &mut written, &mut output)?;
    }
    Ok(())
}

/// Write each reading that differs from what was written last.
fn write_changed(
    service: &dyn Service,
    written: &mut BTreeMap<String, Value>,
    output: &mut impl Write,
) -> std::io::Result<()> {
    for resource in service.resources() {
        let id = resource.id.as_str().to_owned();
        let Some(value) = service.read(&id) else {
            continue;
        };
        if written.get(&id) != Some(&value) {
            let event = Event::Reading {
                resource: id.clone(),
                value: value.clone(),
            };
            writeln!(output, "{}", event.line())?;
            written.insert(id, value);
        }
    }
    output.flush()
}

/// The session's side of a service in another process: the readings as
/// they arrived, the specs built from them, and the line out to it.
pub struct Remote {
    publish: Publish,
    readings: BTreeMap<String, Value>,
    events: Receiver<Event>,
    requests: Box<dyn Write>,
}

impl Remote {
    /// A service reading `events` (parsed from its output, see
    /// [`Remote::events`]) and taking requests on `requests`, its specs built
    /// with `publish`.
    #[must_use]
    pub fn new(publish: Publish, events: Receiver<Event>, requests: Box<dyn Write>) -> Self {
        Self {
            publish,
            readings: BTreeMap::new(),
            events,
            requests,
        }
    }

    /// Parse the service's `output` into events on a thread of its own,
    /// calling `wake` after each.
    ///
    /// # Errors
    /// The thread did not start.
    pub fn events(
        output: impl BufRead + Send + 'static,
        wake: Option<Wake>,
    ) -> std::io::Result<Receiver<Event>> {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("tilo-service-lines".into())
            .spawn(move || {
                for line in output.lines() {
                    let Ok(line) = line else { break };
                    match serde_json::from_str::<Event>(&line) {
                        Ok(event) => {
                            if sender.send(event).is_err() {
                                break;
                            }
                            if let Some(wake) = &wake {
                                wake();
                            }
                        }
                        Err(error) => tracing::debug!(%error, line, "not a service event"),
                    }
                }
            })?;
        Ok(receiver)
    }

    /// Take in one event. Whether a reading changed.
    fn apply(&mut self, event: Event) -> bool {
        match event {
            Event::Reading { resource, value } => {
                self.readings.insert(resource, value.clone()).as_ref() != Some(&value)
            }
            Event::Refused {
                resource,
                action,
                reason,
            } => {
                tracing::warn!(resource, action, reason, "the service refused");
                false
            }
        }
    }

    /// Take in the events that arrived. Whether a reading changed.
    fn take(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed |= self.apply(event);
        }
        changed
    }

    /// Wait until the service wrote its first readings.
    pub fn wait_for_readings(&mut self) {
        while self.readings.is_empty() {
            match self.events.recv() {
                Ok(event) => {
                    self.apply(event);
                }
                Err(_) => return,
            }
        }
        self.take();
    }
}

impl Service for Remote {
    fn resources(&self) -> Vec<ResourceSpec> {
        (self.publish)(&self.readings)
    }

    fn read(&self, resource: &str) -> Option<Value> {
        self.readings.get(resource).cloned()
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let published = self.resources();
        let address = ActionAddress::new(resource, action);
        if tilo_actions::door::resolve(&published, &address).is_none() {
            return Err(DoorError::Unknown(address));
        }
        tilo_actions::door::encode_json(&published, &address, arguments)?;
        let request = Request::Invoke {
            resource: resource.into(),
            action: action.into(),
            arguments: arguments.clone(),
        };
        let line = serde_json::to_string(&request)
            .map_err(|error| DoorError::Failed(error.to_string()))?;
        writeln!(self.requests, "{line}")
            .and_then(|()| self.requests.flush())
            .map_err(|error| DoorError::Failed(format!("the service is gone: {error}")))?;
        Ok(Started::Accepted)
    }

    fn poll(&mut self) -> bool {
        self.take()
    }
}

/// A service in a child process, started by the session: its readings and
/// door as [`Remote`]; dropping it closes the child's input, which stops
/// it, and reaps it.
pub struct Process {
    remote: Remote,
    child: std::process::Child,
}

impl Process {
    /// Start `command` with piped standard input and output and serve what
    /// it writes as a service whose specs `publish` builds, waiting for
    /// its first readings; `wake` is called after each event.
    ///
    /// # Errors
    /// The command did not start, or closed before its first readings.
    pub fn spawn(
        mut command: std::process::Command,
        publish: Publish,
        wake: Option<Wake>,
    ) -> std::io::Result<Self> {
        use std::process::Stdio;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(std::io::Error::other("the service's pipes are missing"));
        };
        let events = Remote::events(std::io::BufReader::new(output), wake)?;
        let mut remote = Remote::new(publish, events, Box::new(input));
        remote.wait_for_readings();
        if remote.readings.is_empty() {
            let _ = child.wait();
            return Err(std::io::Error::other(
                "the service closed before it read anything",
            ));
        }
        Ok(Self { remote, child })
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.remote.requests = Box::new(std::io::sink());
        let _ = self.child.wait();
    }
}

impl Service for Process {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.remote.resources()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        self.remote.read(resource)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        self.remote.invoke(resource, action, arguments)
    }

    fn poll(&mut self) -> bool {
        self.remote.poll()
    }
}
