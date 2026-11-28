//! A device whose daemon runs on an app's own threads, as a phone's does (an Android or iOS app
//! runs no daemon process of its own): the daemon every Tilo computer runs, and what the app's
//! views read of it and ask it, through the API `tilo` itself calls (`tilo login`) and, for the
//! apps a workspace runs, through the daemon's own copy of that workspace: the record its host
//! installed them into and the packages this device fetched from it.
//! Nothing here holds state of its own: the daemon keeps the login, the workspaces and their apps.

mod files;

pub use files::{FileEntry, FileKind, FileWatch};

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::daemon::Running;
use anyhow::{Context as _, Result};
use serde_json::Value;
use tilo_actions::remote::ReceiptStatus;
use tilo_api::calls::{Calls, Reach};
use tilo_device_identity::login::Place;
use tilo_device_identity::persist;
use tilo_local_identity as identity;
use tilo_person_services::person_telling;
use tilo_person_services::person_telling::ToldAction;
use tilo_resource::Effect as _;

/// How long a view waits for the person's services to start in the daemon before their apps read
/// as none: the daemon starts them on its own thread, and the view asks again.
const SERVICES_STARTING: Duration = Duration::from_secs(2);

/// How long listing the person's workspaces waits for their services to start: what a device
/// someone signed in to shows is theirs, so it waits for them rather than show it as no one's.
const PERSON_STARTING: Duration = Duration::from_secs(30);

/// How long one of the person's own actions is waited for: each is answered at once, off a store
/// this device reaches, and a longer wait is a host that is not answering.
const PERSON_ACTION_WAIT_MS: u32 = 10_000;

/// What an app's view reads of the device: whether a person's identity has it, and their
/// workspaces, by id and name.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Devices {
    pub signed_in: bool,
    pub workspaces: Vec<Workspace>,
    /// The person's own workspace, by its key: where a window is before the person picks
    /// another. `None` before anyone signed in.
    pub own: Option<String>,
    /// Why the person's own workspace did not take this device in the last time it was asked,
    /// while it has not.
    pub own_refused: Option<String>,
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

/// What a window of an app whose instance runs in this process reads and writes its records over
/// once a person's identity has the device ([`Device::window_connection`]): the connections the
/// daemon makes for a window it opens, which its collaboration service attests as that window, so
/// the records are the workspace's and reach the person's other devices.
pub struct WindowConnection {
    /// The member at the window: the person the device acts for.
    pub person: String,
    /// The window's instance, as its connection names it.
    pub instance: u64,
    /// The window's connection to the workspace, named as the window of its app in its workspace.
    pub collaboration: runtime_portal::process_link::Descriptor,
    /// The window's connection to the identity service, which signs what it writes as the person.
    pub identity: runtime_portal::process_link::Descriptor,
    /// What the device says of the app's declared actions for the person, handed as a launch
    /// hands them: what the window opens and acts on before it asked anything.
    pub verdicts: Vec<tilo_actions::shown::ActionDecision>,
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
        Self::hold_person();
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
        // The daemon starts the person's services after it starts: a device someone signed in to
        // before it last started (a phone launched again) is asked before they run, and lists
        // their workspaces once they do.
        if let Some(running) = self.running.as_ref() {
            running
                .services(PERSON_STARTING)
                .context("the person's services did not start")?;
        }
        let listed = self
            .runtime
            .block_on(async { Self::calls().await?.workspaces().await })?;
        let workspaces = listed
            .workspaces
            .into_iter()
            .map(|workspace| {
                let key = workspace.id;
                let name = if workspace.name.is_empty() {
                    key.clone()
                } else {
                    workspace.name
                };
                Workspace { key, name }
            })
            .collect();
        let own = Self::own_workspace()?;
        Ok(Devices {
            signed_in: true,
            workspaces,
            own_refused: crate::device::home::refusal(&own),
            own: Some(own),
        })
    }

    /// The person's services, once they run: what a test drives a login through the device's own
    /// actions against, while the part that serves them keeps running.
    #[cfg(test)]
    pub(crate) fn services(
        &self,
        within: Duration,
    ) -> Option<std::sync::Arc<crate::device::services::Daemon>> {
        self.running
            .as_ref()
            .and_then(|running| running.services(within).ok())
    }

    /// The person's own workspace: the one their root names, which the certificate this device
    /// holds of theirs is signed by.
    fn own_workspace() -> Result<String> {
        let place = Self::login_place()?;
        let cert = tilo_device_identity::persist::read_owner_cert(place.keys())
            .context("this device's certificate")?
            .context("this device holds no certificate of its person")?;
        anyhow::ensure!(
            cert.device_public_key == Self::device_key()?,
            "this device's certificate names another device"
        );
        Ok(tilo_server_protocol::servers::person_workspace_id(
            &cert.user_public_key,
        ))
    }

    /// The apps of `workspace`: the packages its host installed into it, which this device's
    /// follower reads too. Each is named by the manifest of the device's own copy when it holds
    /// one, and otherwise by the name its package id makes, as the catalogue names an app whose
    /// manifest gives none.
    ///
    /// # Errors
    /// The daemon answered with a refusal, or the workspace's record of its apps could not be
    /// read.
    pub fn apps(&self, workspace: &str) -> Result<Vec<App>> {
        let Some(running) = self.running.as_ref() else {
            return Ok(Vec::new());
        };
        let services = match running.services(SERVICES_STARTING) {
            Ok(services) => services,
            // The person's services still start, and this device follows nothing yet.
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let installed = self
            .runtime
            .block_on(services.workspaces().app_versions(workspace))
            .context("the apps the workspace runs")?;
        let catalog = app_catalogue::apps::read_from_env().unwrap_or_default();
        let mut apps: Vec<App> = installed
            .into_iter()
            .map(|installed| {
                let id = installed.release.app;
                let found = catalog.iter().find(|app| app.app_id == id.as_str());
                App {
                    // The app's own id, as the session's apps root names each: what
                    // `session.open_app` is given reaches the app the person pressed.
                    key: id.clone(),
                    name: found.map_or_else(
                        || package_store::display_name_for_package_id(&id),
                        |app| app.display_name.clone(),
                    ),
                    icon: ui::icons::app_glyph(&id, found.and_then(|app| app.category.as_deref()))
                        .name()
                        .to_owned(),
                    category: found
                        .and_then(|app| app.category.clone())
                        .unwrap_or_default(),
                }
            })
            .collect();
        apps.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(apps)
    }

    /// The connections a window of `app` in `workspace` running in this process takes, once a
    /// person's identity has the device: the window's records are then the workspace's, kept in
    /// its one store and synced to the person's other devices, as a window the daemon starts is.
    /// Before that the device is no one's, and nothing connects a window to a workspace.
    ///
    /// # Errors
    /// No person's identity has the device yet, the person's services did not start, or the
    /// daemon's services could not be reached.
    pub fn window_connection(&self, app: &str, workspace: &str) -> Result<WindowConnection> {
        if !Self::claimed()? {
            anyhow::bail!("this device is no one's yet: sign it in before an app opens here");
        }
        let running = self
            .running
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("the daemon is stopped"))?;
        let services = running
            .services(SERVICES_STARTING)
            .context("the person's services did not start")?;
        let (person, instance, collaboration, identity) = services
            .apps()
            .window_in_this_process(app, workspace)
            .map_err(anyhow::Error::msg)?;
        let verdicts =
            crate::device::shown::declared(services.workspaces(), &person, workspace, (app, &[]));
        Ok(WindowConnection {
            person,
            instance,
            collaboration,
            identity,
            verdicts,
        })
    }

    /// A call of one of the person's own actions in `workspace`, made as them on this device,
    /// through the one dispatch every call of theirs takes (`tilo.telling`'s, whose answers the
    /// workspace's host keeps, and any action of the workspace itself): the same path the
    /// desktop's shell and a program of the person's reach it by, so what the host decides for
    /// them decides this too.
    ///
    /// # Errors
    /// The person's services did not start, the action was refused or failed, or it did not
    /// answer.
    pub fn person_action(
        &self,
        workspace: &str,
        provider: &str,
        resource: &str,
        action: &str,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<Value> {
        let running = self
            .running
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("the daemon is stopped"))?;
        let services = running
            .services(SERVICES_STARTING)
            .context("the person's services did not start")?;
        let pending = services.person_action(
            workspace,
            provider,
            resource,
            action,
            arguments,
            PERSON_ACTION_WAIT_MS,
        )?;
        self.settled(pending)
    }

    /// The value the call `pending` ends with, or why it did not answer.
    fn settled(
        &self,
        pending: tilo_actions::remote::Pending<
            Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
        >,
    ) -> Result<Value> {
        let receipt = self
            .runtime
            .block_on(pending)
            .ok_or_else(|| anyhow::anyhow!("the action's answer was abandoned"))?
            .map_err(|error| {
                anyhow::anyhow!("the action did not reach the workspace's host: {error}")
            })?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            ReceiptStatus::Failed { error } => Err(anyhow::anyhow!("{error:?}")),
            ReceiptStatus::Rejected { error } => Err(anyhow::anyhow!("{error}")),
            ReceiptStatus::Cancelled { message } => Err(anyhow::anyhow!(
                "{}",
                message.unwrap_or_else(|| "the action was cancelled".to_owned())
            )),
            ReceiptStatus::Accepted => Err(anyhow::anyhow!("the action did not answer in time")),
        }
    }

    /// What the person was told in `workspace` after the place `after`, waiting up to `wait_ms`
    /// for their log to move: their own log, read as one of their devices asks it, on the device
    /// that names itself `device`. Asking is looking, for as long as the ask waits.
    ///
    /// # Errors
    /// As [`Self::person_action`], or the answer does not read.
    pub fn told(
        &self,
        workspace: &str,
        after: u64,
        wait_ms: u64,
        device: &str,
    ) -> Result<person_telling::ToldList> {
        let asking = ToldAction::List {
            after: Some(after),
            wait_ms: Some(wait_ms),
            device: Some(device.to_owned()),
        };
        let answer = self.person_action(
            workspace,
            person_telling::PROVIDER,
            person_telling::TOLD,
            asking.id(),
            asking.arguments(),
        )?;
        serde_json::from_value(answer).context("what the person was told does not read")
    }

    /// Where this device is reached while its app is closed: the web push `endpoint` a phone's
    /// `UnifiedPush` distributor gave it, with the P-256 key its pushes are encrypted for and the
    /// auth secret (`p256dh` and `auth`, in hex), handed over as one of the person's addresses.
    /// Answered with the addresses their devices are reached at.
    ///
    /// # Errors
    /// As [`Self::person_action`], or the address was refused.
    pub fn register_web_push_address(
        &self,
        workspace: &str,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
    ) -> Result<tilo_person_services::push_addresses::AddressList> {
        use tilo_person_services::push_addresses as addresses;
        let register = addresses::Register::WebPush {
            endpoint: endpoint.to_owned(),
            key: addresses::PushKey {
                p256dh: p256dh.to_owned(),
                auth: auth.to_owned(),
            },
        };
        let answer = self.person_action(
            workspace,
            person_telling::PROVIDER,
            addresses::RESOURCE,
            addresses::REGISTER,
            arguments_of(&register)?,
        )?;
        serde_json::from_value(answer)
            .context("where the person's devices are reached does not read")
    }

    /// The person answered the thing `told` on this device: it waits for them no more, and what
    /// showed it elsewhere is taken back.
    ///
    /// # Errors
    /// As [`Self::person_action`].
    pub fn acknowledge_told(
        &self,
        workspace: &str,
        told: &str,
        device: &str,
        by: &str,
    ) -> Result<person_telling::Told> {
        let acknowledged = ToldAction::Acknowledge {
            told: told.to_owned(),
            device: Some(device.to_owned()),
            by: Some(by.to_owned()),
        };
        let answer = self.person_action(
            workspace,
            person_telling::PROVIDER,
            person_telling::TOLD,
            acknowledged.id(),
            acknowledged.arguments(),
        )?;
        serde_json::from_value(answer).context("what was acknowledged does not read")
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
        Self::hold_person();
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
            .and_then(|daemon| daemon.found(&tilo_api::daemon::Founding::default()))
            .map(drop)
            .map_err(anyhow::Error::msg)?;
        self.restart()?;
        Self::hold_person();
        Ok(())
    }

    /// The app's windows act as the person this device's certificate names: no launcher binds
    /// this process to an identity service.
    fn hold_person() {
        let read = Self::login_place()
            .ok()
            .map(|place| persist::read_owner_cert(place.keys()));
        if let Some(Ok(Some(cert))) = read {
            identity::install_source(Some(std::sync::Arc::new(identity::FixedIdentity(
                identity::LocalIdentity {
                    group_public_key: cert.user_public_key,
                    display_name: String::new(),
                    avatar_color: [0; 3],
                    device_public_key: Some(cert.device_public_key),
                },
            ))));
        }
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

    /// This device's public key: the key the daemon in this process took, never read again.
    fn device_key() -> Result<[u8; 32]> {
        tilo_device_identity::persist::device_public_key(&tilo_api::calls::device_keys())
            .context("this device's key")
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

/// `value` as the arguments of an action, which are one JSON object.
fn arguments_of(value: &impl serde::Serialize) -> Result<serde_json::Map<String, Value>> {
    match serde_json::to_value(value).context("the action's arguments")? {
        Value::Object(arguments) => Ok(arguments),
        other => Err(anyhow::anyhow!(
            "the action's arguments are an object, not {other}"
        )),
    }
}
