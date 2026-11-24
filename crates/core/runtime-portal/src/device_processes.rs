//! The device's tilolang processes, as a desktop session reaches them.
//!
//! The device's daemon runs every tilolang process of this computer, one per workspace and app,
//! whether or not anyone is signed in at a desktop. A desktop session is one of its clients: it
//! opens each window's connections (its patch connection, its collaboration connection attested
//! from the session's own launch record, its identity service, its origin connector, the content
//! store, the chooser and the clipboard) and hands them to the daemon with the instance
//! ([`ToDevice::Instance`]), which verifies the app's installation and hands the instance to the
//! process of its workspace and app. What changed for the processes already running is said the
//! same way: the person's formats and language ([`ToDevice::Locale`]) and what they may do in a
//! workspace's apps ([`ToDevice::Access`]).
//!
//! Each ask is one packet socket of its own: one [`ToDevice`] with the descriptors it carries, one
//! [`FromDevice`] back. The session asks for it on the shell link the daemon started it with
//! (`runtime_collab::wire::shell::ShellCall::Processes`), so only that shell hands over: no other
//! process of the account, and none of the apps the daemon started.

use std::io;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tilo_action_policy::MemberAccess;

use crate::packet::{Descriptor, Packet};
use crate::process_link::{AppAccess, Locale, OpenRequest};

/// The launcher's flag for a window whose instance a tilolang process runs.
pub const PROCESS_ARG: &str = "--tilolang-process";

/// What a session asks of the device's processes.
#[derive(Debug, Serialize, Deserialize)]
pub enum ToDevice {
    /// Run `instance` in the process of its workspace and app, starting it when none runs.
    Instance {
        instance: Box<HandedInstance>,
        fds: HandedFds,
    },
    /// The person's formats or language changed: every process running is told.
    Locale(Locale),
    /// What the person may do in apps of `workspace` changed: each process running one of them
    /// there is told.
    Access {
        workspace: String,
        apps: Vec<AppAccess>,
    },
    /// The processes running now.
    Running,
}

impl Packet for ToDevice {}

/// The daemon's answer to a [`ToDevice`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromDevice {
    /// The instance runs: whether its process started for it. `Err` says why it runs nowhere
    /// (the workspace is not the person's here, the app does not verify, no process took it).
    Handed(Result<bool, String>),
    /// The processes running now.
    Running(Vec<Running>),
    /// What changed was said to the processes it is about.
    Told,
}

impl Packet for FromDevice {}

/// One process running: its workspace and app, and the calls it runs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Running {
    pub workspace: String,
    pub app: String,
    pub instances: Vec<u64>,
}

/// One instance, as a session hands it over: the app and workspace it runs in, the window it
/// draws (or none, for a call that shows nothing), who is at it, and what it opens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HandedInstance {
    pub app: String,
    pub workspace: String,
    /// The window's instance: the tab it takes.
    pub instance: u64,
    /// The member at the window, whose steps its undo takes back.
    pub person: String,
    /// The record the window was asked to show, by its id.
    pub document: Option<String>,
    /// The item the window opens on, by its anchor.
    pub reveal: Option<String>,
    /// What the window opens: a file of the content store, or a link.
    pub open: Option<OpenRequest>,
    /// What the person may do in the app, projected from the workspace's policy.
    #[serde(with = "crate::process_link::policy_text")]
    pub access: Option<MemberAccess>,
    /// The person's formats and language, as the session resolved them.
    pub locale: Locale,
    /// Where the workspace's resources open on the web, as JSON, when the session knows it.
    pub link_home: Option<String>,
}

/// The connections a session opened for one instance.
#[derive(Debug, Serialize, Deserialize)]
pub struct HandedFds {
    /// The process's end of the window's patch connection: `None` for a call that shows nothing.
    #[serde(with = "crate::packet::optional_fd")]
    pub patch: Option<Descriptor>,
    /// The instance's collaboration connection, attested as that window of the app's launch, or
    /// as a call that shows nothing.
    #[serde(with = "crate::packet::fd")]
    pub collaboration: Descriptor,
    /// Its identity-service connection, named for the app and workspace.
    #[serde(with = "crate::packet::fd")]
    pub identity: Descriptor,
    /// Its connection to the app's origin connector, when the app has one.
    #[serde(with = "crate::packet::optional_fd")]
    pub origin: Option<Descriptor>,
    /// Its connection to the device's content store, scoped to the app in its workspace.
    #[serde(with = "crate::packet::optional_fd")]
    pub content: Option<Descriptor>,
    /// Its connection to the person's chooser, when the session serves one.
    #[serde(with = "crate::packet::optional_fd")]
    pub chooser: Option<Descriptor>,
    /// Its connection to the person's clipboard, when the session serves one.
    #[serde(with = "crate::packet::optional_fd")]
    pub clipboard: Option<Descriptor>,
    /// Its connection to the session's tools service, made for this app in its workspace.
    #[serde(with = "crate::packet::optional_fd")]
    pub tools: Option<Descriptor>,
}

/// What opens one connection to the device's processes, for one ask.
pub type Connect = dyn Fn() -> io::Result<Descriptor> + Send + Sync;

/// The device's processes, as the person's shell asks them.
#[derive(Clone)]
pub struct Device {
    connect: Arc<Connect>,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Device")
    }
}

impl Device {
    /// The processes each ask reaches on a connection `connect` opens.
    #[must_use]
    pub fn over(connect: Arc<Connect>) -> Self {
        Self { connect }
    }

    /// Run `instance` with its connections `fds` in the process of its workspace and app: whether
    /// its process started for it.
    ///
    /// # Errors
    /// [`io::ErrorKind::PermissionDenied`] with why it runs nowhere, or the connection's error.
    pub fn hand_over(&self, instance: HandedInstance, fds: HandedFds) -> io::Result<bool> {
        let asked = ToDevice::Instance {
            instance: Box::new(instance),
            fds,
        };
        match self.ask(&asked)? {
            FromDevice::Handed(Ok(started)) => Ok(started),
            FromDevice::Handed(Err(refusal)) => {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, refusal))
            }
            other => Err(unexpected(&other)),
        }
    }

    /// Tell every process running that the person's formats or language are now `locale`.
    ///
    /// # Errors
    /// The connection's error.
    pub fn locale_changed(&self, locale: Locale) -> io::Result<()> {
        self.told(&ToDevice::Locale(locale))
    }

    /// Tell the processes of `workspace` running an app of `apps` what the person may now do in it.
    ///
    /// # Errors
    /// The connection's error.
    pub fn access_changed(&self, workspace: &str, apps: Vec<AppAccess>) -> io::Result<()> {
        self.told(&ToDevice::Access {
            workspace: workspace.to_owned(),
            apps,
        })
    }

    /// The processes running now.
    ///
    /// # Errors
    /// The connection's error.
    pub fn running(&self) -> io::Result<Vec<Running>> {
        match self.ask(&ToDevice::Running)? {
            FromDevice::Running(running) => Ok(running),
            other => Err(unexpected(&other)),
        }
    }

    fn told(&self, message: &ToDevice) -> io::Result<()> {
        match self.ask(message)? {
            FromDevice::Told => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Ask the daemon `message` on a connection of its own, and read its answer.
    fn ask(&self, message: &ToDevice) -> io::Result<FromDevice> {
        let connection = (self.connect)()?;
        message.send(&connection)?;
        FromDevice::receive(&connection)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "the device's daemon closed the connection without answering: it takes \
                 hand-overs from the shell it started alone",
            )
        })
    }
}

fn unexpected(answer: &FromDevice) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the device's daemon answered something else: {answer:?}"),
    )
}
