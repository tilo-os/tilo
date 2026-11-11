//! A session in a workspace's VM: the device, the workspace and the window its host handed
//! it (`docs/hosted-browser.md`, "What the session in the VM is handed").
//!
//! The session in a workspace's VM is started at the first browser action, so it
//! holds nothing of its own: the relay placement record on its standard input is the workspace it
//! joins, the record whose Browser window it opens, and the device certificate and key it joins
//! as. It is a device of the workspace like any other — the relay lists it, and its window is
//! published, subscribed and acted on as a window on anyone's computer is — and it does nothing
//! else a person's session does on start: no onboarding, no Lounge, no other app.
//!
//! The session reports what it became on its standard output, one [`Report`] per line: [`READY`]
//! once the identity it was handed is the one it runs as, [`UP`] with the window's provider once
//! the window publishes, and [`USED`] whenever the page is used. The host that started it counts
//! its idle limit from that, and hands the provider to the relay, which routes page actions to it.
//!
//! [`READY`]: tilo_actions::remote::browser::READY
//! [`UP`]: tilo_actions::remote::browser::UP
//! [`USED`]: tilo_actions::remote::browser::USED

use std::io::{self, BufRead as _, Write as _};
use std::time::{Duration, Instant};

use tilo_action_broker::collab_app_broker::{AppBrokerState, ExecutionStatus};
use tilo_actions::remote::browser::{MARK, Report};
use tilo_relay_protocol::hosted::Material;

use crate::Shared;

/// The app whose window a session holds.
pub const BROWSER: &str = "so.tilo.browser";
/// How long the invitation the relay answers the join with stays redeemable.
const ENTER_INVITATION_SECONDS: u64 = 600;
/// How often the page's use is reported while it is in use: one an action or a viewer is what the
/// host counts its idle limit from, and a page being read for an hour is in use the whole time.
const USED_EVERY: Duration = Duration::from_secs(60);
/// The colour the record of the person a session serves carries, as a device that joined
/// without their root gets it.
const ENVIRONMENT_AVATAR: [u8; 3] = [88, 101, 242];

/// What the session reports on its standard output: the host relays every line.
pub type Reports = Box<dyn FnMut(Report) + Send>;

/// The session in a workspace's VM: the identity it was handed, the join it asked of its relay,
/// and the window it holds.
pub struct HostedBrowser {
    /// The relay's placement record: what this session joins, opens and is.
    material: Material,
    reports: Reports,
    /// What wakes the session's loop when the relay answers or the page is used.
    waking: Option<tilo_service::Wake>,
    arrived: std::sync::mpsc::Receiver<Arrived>,
    sender: std::sync::mpsc::Sender<Arrived>,
    join: Join,
    window: Window,
    /// When the page's use was last reported.
    said_used: Option<Instant>,
}

/// What reached the loop from the tasks that watch the relay and the desk.
enum Arrived {
    /// The relay answered the join: the invitation text to redeem, or why it did not.
    Invitation(Result<String, String>),
    /// An action ran on the window's page.
    PageAction,
}

/// How far the join has come.
enum Join {
    /// The relay has not been asked yet.
    Waiting,
    /// The relay was asked, and what it answers is on its way: the invitation is redeemed by the
    /// session's own join, which settles the workspace on the desk.
    Asking,
}

/// The window the session holds.
enum Window {
    /// Nothing was opened: the desk does not hold the workspace yet.
    Unopened,
    /// The launch was made, and no window published for it yet.
    Opening,
    /// The window publishes under this provider, which page actions name: its tab, one number.
    Open(u64),
    /// Browser did not open, and why is in the log: nothing asks again.
    Failed,
}

impl HostedBrowser {
    /// The session, joining as the device `material`, the relay's placement record
    /// names: its identity is taken here, before anything reads this device of itself, and `ready`
    /// is reported. `reports` is where the session says what it became.
    ///
    /// # Errors
    ///
    /// The material is not the relay's, or the identity it names is not one this device may take.
    pub fn take(material: &str, reports: Reports) -> Result<Self, String> {
        let material: Material = serde_json::from_str(material.trim())
            .map_err(|error| format!("the material is not the relay's: {error}"))?;
        take_identity(&material)?;
        let (sender, arrived) = std::sync::mpsc::channel();
        let mut hosted = Self {
            material,
            reports,
            waking: None,
            arrived,
            sender,
            join: Join::Waiting,
            window: Window::Unopened,
            said_used: None,
        };
        hosted.say(Report::Ready);
        Ok(hosted)
    }

    /// Wake the session's loop with `wake` when the relay answers the join or the page is used.
    pub(crate) fn waking(&mut self, wake: tilo_service::Wake) {
        self.waking = Some(wake);
    }

    /// Follow what the session's broker admits: an action on the window's page is the page in use,
    /// and the page track's viewers say so too ([`Self::follow`]).
    pub(crate) fn watching(&self, broker: Option<&std::sync::Arc<AppBrokerState>>) {
        let Some(broker) = broker else {
            return;
        };
        let Ok(runtime) = tilo_rt::try_app() else {
            tracing::warn!("a hosted Browser's page is not watched: no task to watch it on");
            return;
        };
        let mut activity = broker.subscribe_execution_activity();
        let arrived = self.sender.clone();
        let waking = self.waking.clone();
        let app = BROWSER.to_owned();
        let workspace = self.material.workspace.clone();
        runtime.spawn(async move {
            while let Ok(admitted) = activity.recv().await {
                // An execution is reported as it starts and as it ends: the page is in use once it
                // is admitted, whatever became of it.
                if admitted.route.app_id != app
                    || admitted.route.cluster_id != workspace
                    || admitted.status != ExecutionStatus::Accepted
                {
                    continue;
                }
                if arrived.send(Arrived::PageAction).is_err() {
                    return;
                }
                if let Some(wake) = &waking {
                    wake();
                }
            }
        });
    }

    /// What the session does each pass: redeem the invitation the relay answered with, open the
    /// record's window once the desk holds the workspace, and report what became of it.
    pub(crate) fn follow(&mut self, shared: &Shared) {
        self.take_arrived(shared);
        self.join();
        self.open(shared);
        if let Window::Open(provider) = &self.window
            && shared.pages.borrow().viewed().contains(provider)
        {
            self.used();
        }
        self.publish(shared);
    }

    /// Redeem what the relay answered the join with: the invitation text through the session's own
    /// join, the path a pasted invitation takes.
    fn take_arrived(&mut self, shared: &Shared) {
        let arrived: Vec<Arrived> = self.arrived.try_iter().collect();
        for arrived in arrived {
            match arrived {
                Arrived::Invitation(Ok(text)) => {
                    if let Err(reason) =
                        crate::platform::directory::take(&shared.directory_store, &text)
                    {
                        tracing::warn!(%reason, "the workspace was not joined");
                    }
                }
                Arrived::Invitation(Err(reason)) => {
                    tracing::warn!(%reason, "the relay did not let this session into the workspace");
                }
                Arrived::PageAction => self.used(),
            }
        }
    }

    /// Ask its relay to enter the device into the workspace, once: as the device the
    /// material certifies, it answers with an invitation, which the session redeems.
    fn join(&mut self) {
        if !matches!(self.join, Join::Waiting) {
            return;
        }
        self.join = Join::Asking;
        let Ok(runtime) = tilo_rt::try_app() else {
            tracing::warn!("the workspace is not joined: no task to ask the relay on");
            return;
        };
        let material = self.material.clone();
        let arrived = self.sender.clone();
        let waking = self.waking.clone();
        runtime.spawn(async move {
            let answer = invitation(&material).await;
            if arrived.send(Arrived::Invitation(answer)).is_err() {
                return;
            }
            if let Some(wake) = &waking {
                wake();
            }
        });
    }

    /// Open the record's Browser window, once the desk holds the workspace the material names.
    fn open(&mut self, shared: &Shared) {
        if !matches!(self.window, Window::Unopened) {
            return;
        }
        if !held(shared, &self.material.workspace) {
            return;
        }
        let Some(apps) = &shared.launch else {
            return;
        };
        let launch = tilo_app_launch::Launch::Record {
            app: BROWSER.to_owned(),
            record: self.material.record.clone(),
        };
        let workspace = apps.workspace_named(&self.material.workspace);
        match apps.start(&launch, &workspace) {
            Ok(()) => self.window = Window::Opening,
            Err(error) => {
                tracing::warn!(%error, record = %self.material.record, "the session's Browser did not open");
                self.window = Window::Failed;
            }
        }
    }

    /// Report the window's provider once it publishes: the window instance the desk shows the app
    /// in, which is what every page action on it names.
    fn publish(&mut self, shared: &Shared) {
        if !matches!(self.window, Window::Opening) {
            return;
        }
        let Some(provider) = tab_of(shared, &self.material.workspace, BROWSER) else {
            return;
        };
        self.say(Report::Up {
            provider: provider.to_string(),
        });
        self.window = Window::Open(provider);
    }

    /// Report the page in use, no more than once a minute while it is.
    fn used(&mut self) {
        let now = Instant::now();
        if self
            .said_used
            .is_some_and(|said| now.duration_since(said) < USED_EVERY)
        {
            return;
        }
        self.said_used = Some(now);
        self.say(Report::Used);
    }

    /// Say `report` on the session's standard output.
    fn say(&mut self, report: Report) {
        (self.reports)(report);
    }
}

impl crate::Shared {
    /// The session follows its join and its window each pass: this is all it does of
    /// what a person's session does, so what it opened and what it reports is followed here.
    pub(crate) fn follow_hosted_browser(&self) {
        if let Some(hosted) = &mut *self.hosted_browser.borrow_mut() {
            hosted.follow(self);
        }
    }
}

/// The session its host started, when its host says so: what it is
/// started with is the material as the first line of its standard input, and nothing else.
///
/// # Errors
///
/// The material was not read, or names an identity this device may not take.
pub fn handed() -> Result<Option<HostedBrowser>, String> {
    if std::env::var_os(MARK).is_none() {
        return Ok(None);
    }
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| format!("the material was not read from the session's input: {error}"))?;
    if line.trim().is_empty() {
        return Err("the session was handed no material".to_owned());
    }
    HostedBrowser::take(&line, Box::new(|report| say_on_stdout(&report))).map(Some)
}

/// Every report, one line on the session's standard output: what the host that started the
/// session reads, and the log is its stderr only.
fn say_on_stdout(report: &Report) {
    println!("{}", report.line());
    if let Err(error) = std::io::stdout().flush() {
        tracing::warn!(%error, "a hosted Browser's report was not written");
    }
}

/// Take `material`'s device as this session's own: the key it signs with, and the certificate the
/// relay issued for it, where every part of the session that reads this device of itself reads it.
fn take_identity(material: &Material) -> Result<(), String> {
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    paths
        .ensure_directories()
        .map_err(|error| format!("this device's identity has nowhere to live: {error}"))?;
    let key = material.key()?;
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &key)
        .map_err(|error| format!("this device's key was not written: {error}"))?;
    tilo_user_identity::install_device_cert(&paths, &material.device, tilo_clock::unix_now_ns())
        .map_err(|error| format!("this device's certificate was not taken: {error}"))?;
    // The person the workspace belongs to, as a record this device holds of them: the relay hands
    // over their public key, and every part of the session that presents this device's person in a
    // workspace reads that person from this login's identity records. The record carries no root:
    // what signs here is the device certificate the relay issued.
    let directory = runtime_identity::identity_dir_from_env()
        .map_err(|error| format!("this device's identity directory is not known: {error}"))?;
    let record = runtime_identity::public_member_record(
        runtime_identity::UNNAMED_DISPLAY_NAME,
        ENVIRONMENT_AVATAR,
        material.user_public_key,
    )
    .map_err(|error| format!("the person's record was not written: {error}"))?;
    let user = hex::encode(material.user_public_key);
    let id = format!("identity-{}", user.get(..55).unwrap_or(&user));
    if let Err(error) = runtime_identity::import_identity(&directory, &id, &record)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(format!("the person's record was not taken: {error}"));
    }
    Ok(())
}

/// Ask its relay to enter the session's device into the workspace, the way a member
/// device asks to enter a workspace its relay hosts, and answer the invitation text it redeems.
async fn invitation(material: &Material) -> Result<String, String> {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .map_err(|error| format!("the session reached no network: {error}"))?;
    let joined = async {
        let client = tilo_relay_session::RelayClient::connect(
            endpoint.clone(),
            material.relay.clone(),
            material.user_public_key,
            material.device.clone(),
            material.key()?,
            tilo_user_identity::RevocationList::new(),
        )
        .await
        .map_err(|error| format!("the session could not reach its relay: {error}"))?;
        tilo_relay_session::workspaces::enter_invitation(
            &client,
            &material.workspace,
            ENTER_INVITATION_SECONDS,
        )
        .await
    }
    .await;
    endpoint.close().await;
    joined
}

/// Whether the desk holds `workspace`.
fn held(shared: &Shared, workspace: &str) -> bool {
    shared.handle.read(|tree| {
        tree.workspaces
            .iter()
            .any(|held| held.id().to_string() == workspace)
    })
}

/// The tab `app`'s window takes in `workspace`, once the desk holds one: the window's instance,
/// which is the provider its actions are published under.
fn tab_of(shared: &Shared, workspace: &str, app: &str) -> Option<u64> {
    shared.handle.read(|tree| {
        tree.workspaces
            .iter()
            .find(|held| held.id().to_string() == workspace)
            .and_then(|held| {
                held.strips
                    .iter()
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .find(|tab| {
                        tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                            .is_some_and(|shown| shown == app)
                    })
            })
            .map(|tab| tab.id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A session in a workspace's VM, holding the window `provider` and reporting into `said`.
    fn holding(provider: u64, said: &Arc<Mutex<Vec<String>>>) -> HostedBrowser {
        let reports = Arc::clone(said);
        let (sender, arrived) = std::sync::mpsc::channel();
        HostedBrowser {
            material: Material {
                workspace: "11111111-1111-4111-8111-111111111111".into(),
                workspace_id: 1,
                record: "uuid:5c07c075-0000-4000-8000-0000000000aa".into(),
                device: tilo_user_identity::DeviceCert {
                    user_public_key: [1; 32],
                    device_public_key: [2; 32],
                    device_label: "Browser".into(),
                    capabilities: tilo_user_identity::DeviceCapabilities::member(),
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                    issuer: None,
                },
                secret: [0; 32],
                relay: iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[7; 32]).public()),
                user_public_key: [1; 32],
                account: "acct".into(),
            },
            reports: Box::new(move |report| {
                reports
                    .lock()
                    .expect("the reports are this test's")
                    .push(report.line());
            }),
            waking: None,
            arrived,
            sender,
            join: Join::Asking,
            window: Window::Open(provider),
            said_used: None,
        }
    }

    #[test]
    fn the_page_in_use_is_said_once_a_minute_while_it_is() {
        let said: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut hosted = holding(42, &said);
        let reports =
            |said: &Arc<Mutex<Vec<String>>>| said.lock().expect("this test's reports").clone();
        hosted.used();
        assert_eq!(reports(&said), vec!["used".to_owned()], "the page was used");
        hosted.used();
        assert_eq!(
            reports(&said),
            vec!["used".to_owned()],
            "a page in use is said once a minute, not once an action"
        );
        // A minute later it is said again: a page being read for an hour is in use the whole time.
        hosted.said_used = Instant::now().checked_sub(USED_EVERY + Duration::from_secs(1));
        hosted.used();
        assert_eq!(
            reports(&said),
            vec!["used".to_owned(), "used".to_owned()],
            "the page is still in use"
        );
    }
}
