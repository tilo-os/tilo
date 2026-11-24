//! A device whose daemon runs on an app's own threads, as a phone's does (an Android or iOS app
//! runs no daemon process of its own): the daemon every Tilo computer runs, and what the app's
//! views read of it and ask it, through the API `tilo` itself calls (`tilo login`, `tilo ls`).
//! Nothing here holds state of its own: the daemon keeps the login, the workspaces and their apps.

use std::path::{Path, PathBuf};

use crate::daemon::Running;
use anyhow::{Context as _, Result};
use serde_json::Value;
use tilo_api::calls::{Calls, Reach};
use tilo_device_identity::login::Place;

/// What an app's view reads of the device: whether a person's identity has it, and their
/// workspaces, by id and name.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Devices {
    pub signed_in: bool,
    pub workspaces: Vec<Workspace>,
}

/// One of the person's workspaces, keyed as the shell's layout keys its workspaces.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Workspace {
    pub key: String,
    pub name: String,
}

/// One app of a workspace, with the fields the session's `apps` root gives each.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct App {
    pub key: String,
    pub name: String,
    pub icon: String,
    pub category: String,
}

/// The device daemon running in this process, and the runtime its API is called on.
pub struct Device {
    running: Option<Running>,
    profile: PathBuf,
    runtime: tokio::runtime::Runtime,
}

impl Device {
    /// Start this device's daemon on `profile`, with the identity where every Tilo program
    /// keeps it (`TILO_IDENTITY_ROOT`): under the app's own files on a phone.
    ///
    /// # Errors
    /// The daemon did not start, or no runtime for its API calls could be made.
    pub fn start(profile: &Path) -> Result<Self> {
        let running = Running::start(profile).context("the device daemon did not start")?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("tilo-device-api")
            .enable_all()
            .build()
            .context("the device's API runtime")?;
        Ok(Self {
            running: Some(running),
            profile: profile.to_owned(),
            runtime,
        })
    }

    /// This computer's daemon, as `tilo`'s commands call it: at its native endpoint.
    async fn calls() -> Result<Calls> {
        Calls::open(Reach::default()).await
    }

    fn login_place() -> Result<Place> {
        Place::here(tilo_api::calls::device_keys())
    }

    /// Whether a person's identity has this device, and their workspaces. A device no one
    /// signed in to has none, and its daemon serves no API to ask.
    ///
    /// # Errors
    /// The daemon answered with a refusal.
    pub fn devices(&self) -> Result<Devices> {
        if !Self::claimed()? {
            return Ok(Devices::default());
        }
        let listed = self
            .runtime
            .block_on(async { Self::calls().await?.workspaces().await })?;
        let workspaces = listed
            .get("workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|workspace| {
                let key = workspace.get("id")?.as_str()?.to_owned();
                let name = workspace
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&key)
                    .to_owned();
                Some(Workspace { key, name })
            })
            .collect();
        Ok(Devices {
            signed_in: true,
            workspaces,
        })
    }

    /// The apps of `workspace`, as `tilo ls` lists them.
    ///
    /// # Errors
    /// The daemon answered with a refusal.
    pub fn apps(&self, workspace: &str) -> Result<Vec<App>> {
        let listed = self
            .runtime
            .block_on(async { Self::calls().await?.list(Some(workspace), None, None).await })?;
        Ok(listed
            .get("rows")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|row| row.get("kind").and_then(Value::as_str) == Some("app"))
            .filter_map(|row| {
                let key = row.get("name")?.as_str()?.to_owned();
                let name = row
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or(&key)
                    .to_owned();
                Some(App {
                    key,
                    name,
                    icon: String::new(),
                    category: String::new(),
                })
            })
            .collect())
    }

    /// Sign this device in with `ticket`, as `tilo login` does: its daemon logs in
    /// (`devices#logIn`), whether or not a person's identity has the device yet, and its person's
    /// part starts again as the device the login made, entering where the ticket says. The daemon
    /// keeps on running: starting it again would lose where it enters until its copy of the
    /// person's own workspace arrives.
    ///
    /// # Errors
    /// The text is no ticket, the issuing device is not reached, or the ticket is spent or
    /// wrong: what the person is told.
    pub fn log_in(&mut self, ticket: &str, label: &str) -> Result<()> {
        tilo_api::daemon::who::log_in(ticket.trim(), AGENT, label).map_err(anyhow::Error::msg)?;
        Ok(())
    }

    /// Start a new person on this device, as a person's own computer's first command does
    /// (`devices#found`), and start its daemon again to take the person's own workspace up, as
    /// after a login. A device a person's certificate names already goes on as that person: it is
    /// neither founded again nor started again.
    ///
    /// # Errors
    /// The daemon refused or did not answer: what the person is told.
    pub fn found(&mut self) -> Result<()> {
        if Self::claimed()? {
            return Ok(());
        }
        tilo_api::daemon::this_computer()
            .and_then(|daemon| daemon.found(tilo_api::daemon::Founding::default()))
            .map(drop)
            .map_err(anyhow::Error::msg)?;
        self.restart()
    }

    /// Whether this device's identity names a person its daemon serves.
    fn claimed() -> Result<bool> {
        let key = Self::device_key()?;
        Ok(tilo_device_identity::login::claimed(
            &Self::login_place()?,
            &key,
        ))
    }

    /// The daemon that served the device before ends, and another takes the device up, as after
    /// `tilo login`.
    fn restart(&mut self) -> Result<()> {
        if let Some(running) = self.running.take() {
            running.stop()?;
        }
        self.running = Some(Running::start(&self.profile)?);
        Ok(())
    }

    fn device_key() -> Result<[u8; 32]> {
        let (signing, _) = tilo_device_identity::persist::load_or_create_device_key(
            &tilo_api::calls::device_keys(),
        )
        .context("reading this device's key")?;
        Ok(signing.verifying_key().to_bytes())
    }

    /// Stop the daemon: what the app does as it ends.
    ///
    /// # Errors
    /// The daemon did not stop cleanly.
    pub fn stop(mut self) -> Result<()> {
        if let Some(running) = self.running.take() {
            running.stop()?;
        }
        Ok(())
    }
}

/// The agent such a device's login names: the person's own, as `tilo login` names it with no agent
/// given.
const AGENT: &str = "Your agent";
