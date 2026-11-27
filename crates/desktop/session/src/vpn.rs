//! The device VPN this session runs (docs/device-vpn.md): the person's VPN profiles' kill
//! switches, and this device's route through another of their devices.
//!
//! Which profiles block is the device VPN crate's decision
//! ([`device_vpn::profile_block`]), followed from the person's settings and
//! `NetworkManager` and carried out by the root helper that owns the firewall. Whether this
//! device routes through another of the person's devices is decided there too
//! ([`device_vpn::controller`]): the route follows the person's choice and their devices, opens
//! the tunnel this device's daemon carries, asks the helper for the TUN it enters by, and
//! follows what each outcome means — including the route's own kill switch, which keeps traffic
//! in while the tunnel is down instead of letting it out directly. This device is an exit for
//! the person's other devices while they turn it on, and only for their devices: the daemon
//! answers them on this device's endpoint, which is the one endpoint under the device's key
//! ([`device_vpn::link`]).
//!
//! So the tunnel's peer-to-peer half runs in the daemon, which holds the key and the endpoint,
//! and this session keeps what only it may do: it opens the TUN devices through the helper,
//! which answers the seat's compositor alone, and hands the packets over its link to the daemon.
//! Both run for as long as the session does, and both are published as one [`DeviceVpnStatus`]
//! where Settings reads them: next to each profile's switch the row says "blocking traffic until
//! it is" while the block holds, and the VPN list's "Through <device>" row says where the route
//! stands and what it carries. The status belongs to one session: whoever publishes it says
//! everything in it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::platform::fabric::{Speakers, SharedLink};
use device_vpn::controller::{self, AfterEnd, Want};
use device_vpn::link::{self, Control, Frame};
use device_vpn::net::{CLIENT_INTERFACE, MAX_MTU};
use device_vpn::profile_block::{self, Sources, Vpns};
use device_vpn::protocol::{Carrier, ClientLink, ExitLink, Request, Response};
use device_vpn::tunnel::device::TunDevice;
use device_vpn::tunnel::exit::EXIT_MTU;
use device_vpn::tunnel::{Ended, Offer, PacketDevice, client};
use runtime_collab::ShellClient;
use settings_store::device_vpn::{
    self as settings, ClientState, ClientStatus, DeviceVpnStatus, ExitUser, OfferedExit, Problem,
    ProfileBlockStatus, StopRequest, Through,
};
use tilo_lock::MutexRecover as _;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::watch;

/// How long a request to the helper may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
/// The kind of the settings section whose actions change the person's device VPN choices.
pub(crate) const SETTINGS_SECTION: &str = "settings.device-vpn";
/// How often the person's online devices are asked what they offer.
const PROBE_EVERY: Duration = Duration::from_secs(300);
/// How long the route waits before asking the daemon for a tunnel again after its link ended,
/// when the choice did not change: the daemon answers nothing while its shell link is away.
const LINK_RETRY: Duration = Duration::from_secs(1);

/// The person's VPN profile kill switches and their device route, as this session runs them.
pub struct Vpn {
    /// The helper's socket.
    helper: PathBuf,
    /// Where `NetworkManager` 's VPNs come from.
    vpns: Vpns,
}

impl Vpn {
    /// The machine's own: the installed helper and its `NetworkManager` .
    #[must_use]
    pub fn machine() -> Self {
        Self::with(Sources::system())
    }

    /// Against `sources` : a session that is not on the machine the helper runs on follows what it
    /// is given.
    #[must_use]
    pub fn with(sources: Sources) -> Self {
        Self {
            helper: sources.helper,
            vpns: sources.vpns,
        }
    }

    /// Run for the rest of the session: the profile kill switches from now on, and the routes,
    /// which follow the person's devices as the session passes ([`Routes::follow`]).
    pub(crate) fn start(self) -> Routes {
        let publisher = Publisher::default();
        // Published once before anything blocks, so Settings can tell that this session runs even
        // while nothing is blocked and nothing routes.
        publisher.set_profile_block(ProfileBlockStatus::default());
        let blocking = publisher.clone();
        profile_block::start_with(
            move |block| blocking.set_profile_block(block),
            Sources {
                helper: self.helper.clone(),
                vpns: self.vpns,
            },
        );
        Routes {
            helper: self.helper,
            publisher,
            inputs: watch::Sender::new(Inputs::default()),
            stops: watch::Sender::new(None),
            started: std::cell::Cell::new(false),
            follow: std::cell::RefCell::new(Follow::default()),
        }
    }
}

/// The session's side of its route through another of the person's devices, and this device as an
/// exit for theirs: what the person chose, which of their devices could carry it, and the task
/// that carries it out.
pub(crate) struct Routes {
    /// The helper's socket.
    helper: PathBuf,
    /// What this session published last, and the file it wrote.
    publisher: Publisher,
    /// What the route's task follows.
    inputs: watch::Sender<Inputs>,
    /// The last request to stop a device routing through this one.
    stops: watch::Sender<Option<StopRequest>>,
    /// Whether that task runs: it starts with the first pass that has the helper and the link to
    /// the device's daemon.
    started: std::cell::Cell<bool>,
    /// What the session's passes remember between them.
    follow: std::cell::RefCell<Follow>,
}

impl Routes {
    /// Follow the person's choice and their devices for this pass: hand the
    /// route's task what changed, publish what it needs said about them, and
    /// say which device the person's route through one of them must come off,
    /// when that device stopped this one.
    pub(crate) fn follow(
        &self,
        fabric: Option<&crate::platform::fabric::SharedLink>,
        link: Option<&ShellClient>,
    ) -> Option<[u8; 32]> {
        let (Some(fabric), Some(link)) = (fabric, link) else {
            return None;
        };
        let mut follow = self.follow.borrow_mut();
        let generation = settings_store::source().map_or(0, |source| source.revision());
        if follow.settings_generation != Some(generation) {
            follow.settings_generation = Some(generation);
            let section = match settings_store::load().remove(settings::SECTION) {
                Some(serde_json::Value::Object(section)) => section,
                _ => serde_json::Map::new(),
            };
            follow.exit_on = settings::EXIT.from_section(&section);
            follow.through = settings::THROUGH.from_section(&section);
            follow.stop = settings::STOP.from_section(&section);
        }
        self.publisher.set_exit_on(follow.exit_on);
        // This device offers an exit only when it can carry one: the helper that opens its TUN
        // is installed, and the daemon on this session's link answers the tunnel.
        self.publisher.set_exit_available(self.helper.exists());
        if !self.started.get() {
            self.started.set(true);
            self.start(link.clone());
        }

        // The person's devices as their fabric names them: whose name to show beside the route,
        // which of them could carry it, and which of them may route through this one.
        let devices = fabric.known();
        let mut labels = BTreeMap::new();
        let mut online = BTreeSet::new();
        for device in devices.devices().values() {
            let Some(key) = key_of(&device.device) else {
                continue;
            };
            labels.insert(key, device.about.label.clone());
            if device.online {
                online.insert(key);
            }
        }
        self.publisher.set_labels(labels);
        if follow.stop.is_some() && follow.stop_done != follow.stop.as_ref().map(|stop| stop.id) {
            follow.stop_done = follow.stop.as_ref().map(|stop| stop.id);
            self.stops.send_replace(follow.stop.clone());
        }
        let want = follow.through.as_ref().and_then(Want::of);
        self.inputs.send_if_modified(|inputs| {
            let changed =
                inputs.want != want || inputs.online != online || inputs.exit_on != follow.exit_on;
            if changed {
                *inputs = Inputs {
                    want,
                    online,
                    exit_on: follow.exit_on,
                };
            }
            changed
        });
        self.publisher.take_clearing()
    }

    /// Start the task that dials and carries the route, and lends this device's exit, for the rest
    /// of the session.
    fn start(&self, link: ShellClient) {
        let opening = Opening::of(link);
        let helper = self.helper.clone();
        let publisher = self.publisher.clone();
        let inputs = self.inputs.subscribe();
        let probing = inputs.clone();
        let exit = self.inputs.subscribe();
        let stops = self.stops.subscribe();
        let started = std::thread::Builder::new()
            .name("tilo-device-vpn".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(async move {
                        tokio::join!(
                            drive(&helper, &opening, &publisher, inputs),
                            probe_exits(&opening, &publisher, probing),
                            hold_exit(&helper, &opening, &publisher, exit, stops),
                        );
                    }),
                    Err(error) => {
                        tracing::warn!(%error, "could not start the device VPN");
                    }
                }
            });
        if let Err(error) = started {
            tracing::warn!(%error, "could not start the device VPN");
        }
    }
}

/// What the session's passes remember of the person's choices.
#[derive(Default)]
struct Follow {
    /// The settings document as the choices were last read.
    settings_generation: Option<u64>,
    exit_on: bool,
    through: Option<Through>,
    /// The last request to stop a device routing through this one.
    stop: Option<StopRequest>,
    stop_done: Option<u64>,
}

/// What the route's task follows: the person's choice, their devices with a session right now,
/// and whether this device's exit is on.
#[derive(Clone, Default)]
struct Inputs {
    want: Option<Want>,
    online: BTreeSet<[u8; 32]>,
    exit_on: bool,
}

/// The key a device is dialled by, from the id its fabric knows it by.
fn key_of(device: &crate::platform::fabric::DeviceId) -> Option<[u8; 32]> {
    let mut key = [0_u8; 32];
    hex::decode_to_slice(device.as_str().trim(), &mut key).ok()?;
    Some(key)
}

/// What this session published of its device VPN, and the file it wrote.
/// The kill switches and the route report from their own threads; every part is written out as one
/// [`DeviceVpnStatus`], since Settings reads one file and a part written alone would say the rest
/// is off. The first report of any part writes, even when the status is all defaults: Settings
/// reads it to know this session runs.
#[derive(Clone, Default)]
struct Publisher(Arc<Mutex<Published>>);

#[derive(Default)]
struct Published {
    profile_block: ProfileBlockStatus,
    exit_available: bool,
    exit_on: bool,
    /// The devices routing through this one, by their key, and since when.
    exit_users: Vec<([u8; 32], u64)>,
    labels: BTreeMap<[u8; 32], String>,
    offered: Vec<[u8; 32]>,
    client: Option<(Want, ClientState)>,
    /// This session's lease on the status file, and the status as last written.
    lease: Option<(u32, u64)>,
    written: Option<DeviceVpnStatus>,
    /// The device that stopped this one, which the session's next pass takes out of the person's
    /// choice: the route's task cannot change a setting itself, since a published action is the
    /// session's to invoke.
    clearing: Option<[u8; 32]>,
}

impl Publisher {
    /// Leave `device` for the session's next pass to take out of the person's choice: it stopped
    /// this one.
    fn clear_through(&self, device: [u8; 32]) {
        self.0.lock_recover().clearing = Some(device);
    }

    /// The device to take out of the person's choice, once.
    fn take_clearing(&self) -> Option<[u8; 32]> {
        self.0.lock_recover().clearing.take()
    }

    /// What the person's VPN profiles block, as the kill switch decided.
    fn set_profile_block(&self, block: ProfileBlockStatus) {
        let mut published = self.0.lock_recover();
        if published.written.is_some() && published.profile_block == block {
            return;
        }
        published.profile_block = block;
        published.write();
    }

    /// Whether this device shares its connection, as the person set it.
    fn set_exit_on(&self, on: bool) {
        let mut published = self.0.lock_recover();
        if published.written.is_some() && published.exit_on == on {
            return;
        }
        published.exit_on = on;
        published.write();
    }

    /// Whether this session can be an exit at all: the helper is installed and its daemon answers
    /// the tunnel.
    fn set_exit_available(&self, available: bool) {
        let mut published = self.0.lock_recover();
        if published.written.is_some() && published.exit_available == available {
            return;
        }
        published.exit_available = available;
        published.write();
    }

    /// The devices routing through this one now, and since when.
    fn set_exit_users(&self, users: Vec<([u8; 32], u64)>) {
        let mut published = self.0.lock_recover();
        if published.written.is_some() && published.exit_users == users {
            return;
        }
        published.exit_users = users;
        published.write();
    }

    /// What the person calls their devices.
    fn set_labels(&self, labels: BTreeMap<[u8; 32], String>) {
        let mut published = self.0.lock_recover();
        if published.written.is_some() && published.labels == labels {
            return;
        }
        published.labels = labels;
        published.write();
    }

    /// Where the route stands, or that it is gone.
    fn set_client(&self, client: Option<(Want, ClientState)>) {
        let mut published = self.0.lock_recover();
        if published.written.is_some() && published.client == client {
            return;
        }
        published.client = client;
        published.write();
    }

    /// The person's devices this one could route through.
    fn set_offered(&self, offered: Vec<[u8; 32]>) {
        let mut published = self.0.lock_recover();
        if published.written.is_some() && published.offered == offered {
            return;
        }
        published.offered = offered;
        published.write();
    }
}

impl Published {
    /// The status as it stands.
    fn status(&self) -> DeviceVpnStatus {
        let label = |device: &[u8; 32]| self.labels.get(device).cloned().unwrap_or_default();
        DeviceVpnStatus {
            exit_available: self.exit_available,
            exit_on: self.exit_on,
            exit_users: self
                .exit_users
                .iter()
                .map(|(device, since_unix_ms)| ExitUser {
                    device: hex::encode(device),
                    label: label(device),
                    since_unix_ms: *since_unix_ms,
                })
                .collect(),
            offered: self
                .offered
                .iter()
                .map(|device| OfferedExit {
                    device: hex::encode(device),
                    label: label(device),
                })
                .collect(),
            client: self.client.as_ref().map(|(want, state)| ClientStatus {
                device: hex::encode(want.device),
                label: label(&want.device),
                state: state.clone(),
                routes: want.routes,
                kill_switch: want.kill_switch,
            }),
            profile_block: self.profile_block.clone(),
            ..DeviceVpnStatus::default()
        }
    }

    /// Write the status where Settings reads it, when it changed.
    fn write(&mut self) {
        if self.lease.is_none() {
            self.lease = settings_store::hardware::publisher_identity().ok();
        }
        let Some((pid, ticks)) = self.lease else {
            return;
        };
        let mut status = self.status();
        status.publisher_pid = pid;
        status.publisher_start_ticks = ticks;
        if self.written.as_ref() == Some(&status) {
            return;
        }
        if let Err(error) = status.save() {
            tracing::warn!(%error, "could not publish the device VPN's status");
            return;
        }
        self.written = Some(status);
    }
}

/// Ask the helper; the answer may carry a TUN.
async fn ask_helper(
    helper: &Path,
    request: Request,
) -> Result<(Response, Option<std::os::fd::OwnedFd>), Problem> {
    let helper = helper.to_path_buf();
    tokio::task::spawn_blocking(move || {
        device_vpn::protocol::call(&helper, &request, HELPER_TIMEOUT)
    })
    .await
    .map_err(|_| Problem::Setup)?
    .map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            Problem::NotInstalled
        }
        _ => Problem::Setup,
    })
}

/// The client's link while the helper holds it.
struct Held {
    want: Want,
    link: ClientLink,
    device: Arc<TunDevice>,
}

/// One tunnel's link to the daemon, both ways.
type Reader = ReadHalf<tokio::net::UnixStream>;
type Writer = WriteHalf<tokio::net::UnixStream>;

/// Opens a link to this device's daemon: the daemon's own, or a test's.
type Open = Arc<
    dyn Fn() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Option<tokio::net::UnixStream>> + Send>,
        > + Send
        + Sync,
>;

/// How the session opens a link to this device's daemon: the daemon's own, or a test's.
#[derive(Clone)]
struct Opening(Open);

impl std::fmt::Debug for Opening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Opening")
    }
}

impl Opening {
    /// The links the daemon gave this shell: one per tunnel, opened fresh.
    fn of(link: ShellClient) -> Self {
        Self(Arc::new(move || {
            let link = link.clone();
            Box::pin(async move { link.device_vpn().await.ok() })
        }))
    }

    async fn open(&self) -> Option<tokio::net::UnixStream> {
        (self.0)().await
    }
}

fn set_state(publisher: &Publisher, want: &Want, state: ClientState) {
    publisher.set_client(Some((want.clone(), state)));
}

/// Resolves once the person's choice is no longer `want` . Never, once nobody can change it.
async fn choice_changes(inputs: &mut watch::Receiver<Inputs>, want: &Want) {
    loop {
        if inputs.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
        if inputs.borrow().want.as_ref() != Some(want) {
            return;
        }
    }
}

async fn release(helper: &Path, held: &mut Option<Held>) {
    if held.take().is_some() {
        close_client_link(helper).await;
    }
}

/// Undo this device's route with the helper: its TUN and the rules that put traffic in it.
async fn close_client_link(helper: &Path) {
    if let Err(problem) = ask_helper(helper, Request::CloseClient).await {
        tracing::warn!(?problem, "could not undo this device's route");
    }
}

async fn open_link(helper: &Path, link: &ClientLink) -> Result<TunDevice, Problem> {
    match ask_helper(helper, Request::OpenClient { link: link.clone() }).await? {
        (Response::Opened { .. }, Some(fd)) => TunDevice::new(fd).map_err(|_| Problem::Setup),
        (answer, _) => {
            tracing::warn!(?answer, "the helper did not open this device's route");
            Err(Problem::Setup)
        }
    }
}

/// One ask of the daemon on a fresh link: the tunnel is carried on it, so every dial is its own.
async fn ask_daemon(link: &Opening, control: &Control) -> Option<(Reader, Writer, Control)> {
    let stream = link.open().await?;
    let (mut reader, mut writer) = tokio::io::split(stream);
    link::write_control(&mut writer, control).await.ok()?;
    match link::read(&mut reader).await.ok()? {
        Frame::Control(answer) => Some((reader, writer, answer)),
        Frame::Packet(_) => None,
    }
}

/// What the daemon answered a route's ask with.
enum Routed {
    /// The tunnel is up: what the exit offers, the TUN's MTU, and what must reach the exit outside
    /// the tunnel.
    Up {
        offer: Offer,
        mtu: u16,
        carrier: Carrier,
        reader: Reader,
        writer: Writer,
    },
    /// The exit would not carry this device.
    Refused(client::Refusal),
    /// The exit was not reached, or the daemon could not ask it.
    Unreachable(String),
}

/// Ask the daemon to route this device through `want`'s exit.
async fn route_through(link: &Opening, want: &Want) -> Routed {
    let ask = Control::Through {
        exit: hex::encode(want.device),
        at: None,
    };
    let Some((reader, writer, answer)) = ask_daemon(link, &ask).await else {
        return Routed::Unreachable("the device's daemon did not answer".to_owned());
    };
    match answer {
        Control::Offer {
            offer,
            mtu,
            carrier,
        } => Routed::Up {
            offer,
            mtu,
            carrier,
            reader,
            writer,
        },
        Control::Refused(refusal) => Routed::Refused(refusal),
        Control::Failed(why) => Routed::Unreachable(why),
        other => Routed::Unreachable(format!("the daemon answered a route with {other:?}")),
    }
}

/// Do what `after` says, then wait as long as it says or until the choice changes.
async fn settle(
    helper: &Path,
    publisher: &Publisher,
    held: &mut Option<Held>,
    want: &Want,
    after: AfterEnd,
    inputs: &mut watch::Receiver<Inputs>,
) {
    if !after.keep_link {
        release(helper, held).await;
    }
    if after.clear_through {
        // The device this one routes through stopped it: the session takes it out of the person's
        // choice on its next pass, through the settings action, since their settings change through
        // their actions.
        publisher.clear_through(want.device);
    }
    set_state(publisher, want, after.state);
    match after.retry {
        Some(delay) => {
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = choice_changes(inputs, want) => {}
            }
        }
        None => choice_changes(inputs, want).await,
    }
}

/// The helper's link for `offer` : the one held when it still fits, else a new one. On failure,
/// what to do instead.
async fn link_up(
    helper: &Path,
    want: &Want,
    offer: &Offer,
    address: std::net::Ipv4Addr,
    mtu: u16,
    carrier: Carrier,
    held: &mut Option<Held>,
    attempt: u32,
) -> Result<Arc<TunDevice>, AfterEnd> {
    let keep_link = held.is_some() && want.kill_switch;
    let link = controller::link_for(want, address, mtu, offer, carrier).map_err(|reason| {
        tracing::info!(%reason, "no route through the other device");
        AfterEnd {
            keep_link,
            clear_through: false,
            retry: Some(controller::REFUSED_RETRY),
            state: ClientState::Failed {
                problem: Problem::NoHomeNetwork,
            },
        }
    })?;
    if let Some(held) = held
        .as_ref()
        .filter(|held| held.link == link && held.want == *want)
    {
        return Ok(Arc::clone(&held.device));
    }
    let device = open_link(helper, &link).await.map_err(|problem| AfterEnd {
        keep_link,
        clear_through: false,
        retry: Some(controller::backoff(attempt)),
        state: ClientState::Failed { problem },
    })?;
    let device = Arc::new(device);
    *held = Some(Held {
        want: want.clone(),
        link,
        device: Arc::clone(&device),
    });
    Ok(device)
}

/// Carry this device's packets between its TUN and the daemon's link until the tunnel ends.
async fn carry(mut reader: Reader, mut writer: Writer, device: Arc<TunDevice>) -> Ended {
    let mut buf = vec![0_u8; usize::from(MAX_MTU)];
    loop {
        tokio::select! {
            read = device.recv(&mut buf) => {
                let length = match read {
                    Ok(length) => length,
                    Err(error) => return Ended::Lost(format!("the tunnel device: {error}")),
                };
                let Some(packet) = buf.get(..length) else { continue };
                if let Err(error) = link::write_packet(&mut writer, packet).await {
                    return Ended::Lost(format!("the tunnel's link: {error}"));
                }
            }
            frame = link::read(&mut reader) => match frame {
                Ok(Frame::Packet(packet)) => {
                    if let Err(error) = device.send(&packet).await {
                        return Ended::Lost(format!("the tunnel device: {error}"));
                    }
                }
                Ok(Frame::Control(Control::Ended(ended))) => return ended,
                Ok(_) => {}
                Err(error) => return Ended::Lost(format!("the tunnel's link: {error}")),
            },
        }
    }
}

/// One attempt to route through `want`'s exit: dial it, open this device's side of the tunnel and
/// say what carries the packets; `None` when the attempt was settled and the next one begins.
async fn attempt_route(
    helper: &Path,
    link: &Opening,
    publisher: &Publisher,
    want: &Want,
    held: &mut Option<Held>,
    inputs: &mut watch::Receiver<Inputs>,
    attempt: u32,
) -> Option<(Reader, Writer, Arc<TunDevice>)> {
    let routed = tokio::select! {
        routed = route_through(link, want) => routed,
        () = choice_changes(inputs, want) => return None,
    };
    let (offer, address, mtu, carrier, reader, writer) = match routed {
        Routed::Up {
            offer,
            mtu,
            carrier,
            reader,
            writer,
        } => {
            let Some(address) = offer.address else {
                let after = controller::after_failed_dial(
                    &client::ClientError::Refused(client::Refusal::Protocol),
                    held.is_some(),
                    want.kill_switch,
                    attempt,
                );
                settle(helper, publisher, held, want, after, inputs).await;
                return None;
            };
            (offer, address, mtu, carrier, reader, writer)
        }
        Routed::Refused(refusal) => {
            let after = controller::after_failed_dial(
                &client::ClientError::Refused(refusal),
                held.is_some(),
                want.kill_switch,
                attempt,
            );
            settle(helper, publisher, held, want, after, inputs).await;
            return None;
        }
        Routed::Unreachable(why) => {
            let after = controller::after_failed_dial(
                &client::ClientError::Unreachable(why),
                held.is_some(),
                want.kill_switch,
                attempt,
            );
            settle(helper, publisher, held, want, after, inputs).await;
            return None;
        }
    };
    match link_up(helper, want, &offer, address, mtu, carrier, held, attempt).await {
        Ok(device) => Some((reader, writer, device)),
        Err(after) => {
            settle(helper, publisher, held, want, after, inputs).await;
            None
        }
    }
}

/// Route through the device the person chose, for as long as they choose it.
async fn drive(
    helper: &Path,
    link: &Opening,
    publisher: &Publisher,
    mut inputs: watch::Receiver<Inputs>,
) {
    let mut held: Option<Held> = None;
    let mut attempt: u32 = 0;
    // A route a crashed session left with its kill switch: undone unless the person still wants it,
    // when the new link replaces it.
    let mut stale = Path::new("/sys/class/net").join(CLIENT_INTERFACE).exists();
    loop {
        let current = inputs.borrow_and_update().clone();
        if controller::link_must_go(held.as_ref().map(|held| &held.want), current.want.as_ref()) {
            release(helper, &mut held).await;
        }
        let Some(want) = current.want.clone() else {
            if std::mem::take(&mut stale) {
                close_client_link(helper).await;
            }
            publisher.set_client(None);
            attempt = 0;
            if inputs.changed().await.is_err() {
                return;
            }
            continue;
        };
        stale = false;
        attempt = attempt.saturating_add(1);
        let blocking = held.is_some() && want.kill_switch;
        set_state(
            publisher,
            &want,
            if held.is_some() {
                ClientState::Reconnecting { blocking }
            } else {
                ClientState::Connecting
            },
        );
        let Some((reader, writer, device)) = attempt_route(
            helper,
            link,
            publisher,
            &want,
            &mut held,
            &mut inputs,
            attempt,
        )
        .await
        else {
            continue;
        };
        attempt = 0;
        set_state(
            publisher,
            &want,
            ClientState::Connected {
                since_unix_ms: tilo_clock::unix_now_ms(),
            },
        );
        let ended = tokio::select! {
            ended = carry(reader, writer, device) => ended,
            () = choice_changes(&mut inputs, &want) => Ended::Closed,
        };
        tracing::info!(?ended, "the route through another device ended");
        let after = controller::after_end(&ended, want.kill_switch, 1);
        settle(helper, publisher, &mut held, &want, after, &mut inputs).await;
    }
}

/// Learn which of the person's online devices offer themselves: when the online devices change, and
/// every [`PROBE_EVERY`].
async fn probe_exits(link: &Opening, publisher: &Publisher, mut inputs: watch::Receiver<Inputs>) {
    let mut asked: Option<(BTreeSet<[u8; 32]>, std::time::Instant)> = None;
    loop {
        let current = inputs.borrow_and_update().clone();
        let due = asked
            .as_ref()
            .is_none_or(|(online, at)| *online != current.online || at.elapsed() >= PROBE_EVERY);
        if due {
            let mut offered = Vec::new();
            for device in &current.online {
                let ask = Control::Probe {
                    exit: hex::encode(device),
                    at: None,
                };
                let Some((_, _, answer)) = ask_daemon(link, &ask).await else {
                    continue;
                };
                if matches!(answer, Control::Offer { .. }) {
                    offered.push(*device);
                }
            }
            publisher.set_offered(offered);
            asked = Some((current.online.clone(), std::time::Instant::now()));
        }
        tokio::select! {
            changed = inputs.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            () = tokio::time::sleep(PROBE_EVERY) => {}
        }
    }
}

/// Carry one lent exit's packets both ways until the tunnel, the exit or the person's choice ends
/// it: whether the session ends with it, rather than the exit opening again.
async fn carry_exit(
    opened: &TunDevice,
    reader: &mut Reader,
    writer: &mut Writer,
    since: &mut BTreeMap<[u8; 32], u64>,
    publisher: &Publisher,
    stops: &mut watch::Receiver<Option<StopRequest>>,
    inputs: &mut watch::Receiver<Inputs>,
) -> bool {
    let mut buf = vec![0_u8; usize::from(MAX_MTU)];
    loop {
        tokio::select! {
            read = opened.recv(&mut buf) => {
                let length = match read {
                    Ok(length) => length,
                    Err(error) => {
                        tracing::info!(%error, "this device's exit tunnel closed");
                        break;
                    }
                };
                let Some(packet) = buf.get(..length) else { continue };
                if let Err(error) = link::write_packet(writer, packet).await {
                    tracing::info!(%error, "this device's exit packets stop here");
                    break;
                }
            }
            frame = link::read(reader) => match frame {
                Ok(Frame::Packet(packet)) => {
                    if let Err(error) = opened.send(&packet).await {
                        tracing::info!(%error, "the exit's tunnel takes no more packets");
                        break;
                    }
                }
                Ok(Frame::Control(Control::Users { devices })) => {
                    let now = tilo_clock::unix_now_ms();
                    let mut users = Vec::new();
                    for device in &devices {
                        if let Some(key) = key_hex(device) {
                            let entered = *since.entry(key).or_insert(now);
                            users.push((key, entered));
                        }
                    }
                    since.retain(|key, _| users.iter().any(|(user, _)| user == key));
                    publisher.set_exit_users(users);
                }
                Ok(_) => {}
                Err(_) => break,
            },
            stop = stops.changed() => {
                if stop.is_err() {
                    return true;
                }
                let Some(stop) = stops.borrow_and_update().clone() else { continue };
                let said = Control::Stop { device: stop.device };
                if let Err(error) = link::write_control(writer, &said).await {
                    tracing::info!(%error, "the exit link takes no more asks");
                    break;
                }
            }
            changed = inputs.changed() => {
                if changed.is_err() || !inputs.borrow().exit_on {
                    break;
                }
            }
        }
    }
    false
}

/// This device as an exit for the person's other devices: while they turn it on, open the exit TUN
/// through the helper and lend it to the daemon, which answers their devices on this device's
/// endpoint; say who routes through it, and stop the ones the person stopped from here.
async fn hold_exit(
    helper: &Path,
    link: &Opening,
    publisher: &Publisher,
    mut inputs: watch::Receiver<Inputs>,
    mut stops: watch::Receiver<Option<StopRequest>>,
) {
    let mut since: BTreeMap<[u8; 32], u64> = BTreeMap::new();
    loop {
        while !inputs.borrow_and_update().exit_on {
            publisher.set_exit_users(Vec::new());
            since.clear();
            if inputs.changed().await.is_err() {
                return;
            }
        }
        let opened = match open_exit(helper).await {
            Ok(device) => device,
            Err(problem) => {
                tracing::info!(?problem, "this device's exit has no tunnel");
                publisher.set_exit_users(Vec::new());
                tokio::select! {
                    changed = inputs.changed() => if changed.is_err() { return },
                    () = tokio::time::sleep(controller::REFUSED_RETRY) => {}
                }
                continue;
            }
        };
        let where_it_stands = {
            let ask = Control::Exit;
            ask_daemon(link, &ask).await
        };
        let Some((mut reader, mut writer, answer)) = where_it_stands else {
            close_exit(helper).await;
            tokio::select! {
                changed = inputs.changed() => if changed.is_err() { return },
                () = tokio::time::sleep(LINK_RETRY) => {}
            }
            continue;
        };
        if !matches!(answer, Control::Ready) {
            // This daemon serves no exit, or its TUN is taken: Settings stops offering the
            // switch rather than promising what this session cannot carry.
            tracing::info!(?answer, "the daemon did not take this device's exit");
            publisher.set_exit_available(false);
            close_exit(helper).await;
            tokio::select! {
                changed = inputs.changed() => if changed.is_err() { return },
                () = tokio::time::sleep(controller::REFUSED_RETRY) => {}
            }
            continue;
        }
        publisher.set_exit_available(true);
        if carry_exit(
            &opened,
            &mut reader,
            &mut writer,
            &mut since,
            publisher,
            &mut stops,
            &mut inputs,
        )
        .await
        {
            return;
        }
        publisher.set_exit_users(Vec::new());
        since.clear();
        close_exit(helper).await;
    }
}

/// The device a hex id names.
fn key_hex(device: &str) -> Option<[u8; 32]> {
    let mut key = [0_u8; 32];
    hex::decode_to_slice(device.trim(), &mut key).ok()?;
    Some(key)
}

/// Open this device's exit TUN through the helper: the interface the person's other devices route
/// through.
async fn open_exit(helper: &Path) -> Result<Arc<TunDevice>, Problem> {
    match ask_helper(
        helper,
        Request::OpenExit {
            link: ExitLink { mtu: EXIT_MTU },
        },
    )
    .await?
    {
        (Response::Opened { .. }, Some(fd)) => {
            TunDevice::new(fd).map(Arc::new).map_err(|_| Problem::Setup)
        }
        (answer, _) => {
            tracing::warn!(?answer, "the helper did not open this device's exit");
            Err(Problem::Setup)
        }
    }
}

async fn close_exit(helper: &Path) {
    if let Err(problem) = ask_helper(helper, Request::CloseExit).await {
        tracing::warn!(?problem, "could not close this device's exit");
    }
}

impl crate::Shared {
    /// Follow where this device routes through another of the person's devices, and what it carries
    /// for them: the route follows what they chose on the pass they chose it on, and a device that
    /// stopped this one takes their choice off through the action that made it.
    pub(crate) fn follow_device_vpn(&self, fabric: &SharedLink) {
        let Some(routes) = &self.vpn else {
            return;
        };
        let Some(stopped) = routes.follow(Some(fabric), self.link.as_ref()) else {
            return;
        };
        // The device this one routes through stopped it: the person's choice
        // comes off through the same action their own choice goes through,
        // and the route's task follows it on the next pass.
        let mut arguments = serde_json::Map::new();
        arguments.insert("device".into(), hex::encode(stopped).into());
        arguments.insert("on".into(), false.into());
        let invoked = self.services.borrow_mut().invoke(
            &settings_sections::address(SETTINGS_SECTION).unwrap_or_default(),
            "route-through",
            &arguments,
        );
        if let Some(Err(error)) = invoked {
            tracing::warn!(%error, "could not clear the device this one routes through");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufRead as _;
    use std::net::SocketAddrV4;
    use std::os::fd::AsFd as _;
    use std::os::unix::net::{UnixDatagram, UnixListener};

    use device_vpn::fdpass;
    use device_vpn::packet;
    use device_vpn::protocol::{Envelope, PROTOCOL_VERSION, Response};

    use super::*;

    /// A helper at `socket` answering every request as carried out, and the requests it was
    /// asked, in order.
    fn helper_at(socket: &Path, tuns: Arc<Mutex<Vec<UnixDatagram>>>) -> Arc<Mutex<Vec<Request>>> {
        let listener = UnixListener::bind(socket).expect("the helper listens");
        let asked: Arc<Mutex<Vec<Request>>> = Arc::default();
        let recorded = Arc::clone(&asked);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("a stream"));
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let envelope: Envelope = serde_json::from_str(&line).expect("a request");
                assert_eq!(envelope.protocol, PROTOCOL_VERSION);
                recorded.lock().unwrap().push(envelope.request.clone());
                match envelope.request {
                    Request::OpenClient { link } => opened(&stream, &tuns, link.mtu),
                    Request::OpenExit { link } => opened(&stream, &tuns, link.mtu),
                    _ => {
                        std::io::Write::write_all(&mut stream, b"{\"result\":\"done\"}\n")
                            .expect("the answer");
                    }
                }
            }
        });
        asked
    }

    /// Answer one `OpenClient` or `OpenExit` with a tunnel the test holds the other end of.
    fn opened(
        stream: &std::os::unix::net::UnixStream,
        tuns: &Arc<Mutex<Vec<UnixDatagram>>>,
        mtu: u16,
    ) {
        let (ours, theirs) = UnixDatagram::pair().expect("a tunnel");
        tuns.lock().unwrap().push(ours);
        let answer = serde_json::to_string(&Response::Opened {
            interface: "tilovpn-test".to_owned(),
            mtu,
        })
        .expect("an answer");
        fdpass::write_line_with_fd(
            stream,
            format!(
                "{answer}
"
            )
            .as_bytes(),
            Some(theirs.as_fd()),
        )
        .expect("the opened answer");
    }

    /// The daemon's end of one link: it answers the ask with `answer`, keeps what the session
    /// writes for the test, and writes what the test hands it.
    async fn daemon_side(
        stream: tokio::net::UnixStream,
        answer: Control,
        heard: tokio::sync::mpsc::UnboundedSender<Frame>,
        mut write: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    ) {
        let (mut reader, mut writer) = tokio::io::split(stream);
        if link::write_control(&mut writer, &answer).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                frame = link::read(&mut reader) => match frame {
                    Ok(frame) => {
                        if heard.send(frame).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                packet = write.recv() => match packet {
                    Some(packet) => {
                        if link::write_packet(&mut writer, &packet).await.is_err() {
                            return;
                        }
                    }
                    None => return,
                },
            }
        }
    }

    /// What this device asked the helper for, once the route is wanted: the network it is
    /// for, the address the other device gave, and the carrier its own packets leave by.
    fn the_route_is_asked_for(asked: &Mutex<Vec<Request>>, address: std::net::Ipv4Addr) {
        let Some(Request::OpenClient { link }) = asked
            .lock()
            .unwrap()
            .iter()
            .find(|request| matches!(request, Request::OpenClient { .. }))
            .cloned()
        else {
            panic!("the helper was asked for this device's tunnel");
        };
        assert_eq!(
            link.routes,
            device_vpn::protocol::Routes::Networks {
                networks: vec!["192.168.1.0/24".parse().expect("a network")],
            },
            "only the network the other device is on goes through it"
        );
        assert_eq!(link.address, address, "the address the other device gave");
        assert!(link.dns, "names resolve on the other device's network");
        assert_eq!(
            link.mtu,
            device_vpn::tunnel::exit::EXIT_MTU,
            "the path's MTU"
        );
        assert!(
            !link.carrier.udp_ports.is_empty(),
            "the tunnel's own packets reach the exit outside the tunnel"
        );
    }

    /// One packet each way on the opened tunnel: what this device's stack writes reaches the
    /// daemon as it was written, and what the exit sends comes out of the tunnel.
    async fn carried(
        address: std::net::Ipv4Addr,
        tuns: &Mutex<Vec<UnixDatagram>>,
        from_session: &mut tokio::sync::mpsc::UnboundedReceiver<Frame>,
        to_session: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    ) {
        let tun = tuns
            .lock()
            .unwrap()
            .first()
            .expect("the helper opened a tunnel")
            .try_clone()
            .expect("a tunnel");
        let web = SocketAddrV4::new(std::net::Ipv4Addr::new(198, 51, 100, 7), 443);
        let outward = packet::udp_packet(SocketAddrV4::new(address, 50_000), web, b"hello")
            .expect("a packet to send through it");
        tun.send(&outward).expect("the packet goes into the tunnel");
        // The ask this link opened with is no packet: what the test waits for is the packet.
        let seen = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(frame) = from_session.recv().await {
                if let Frame::Packet(packet) = frame {
                    return Some(packet);
                }
            }
            None
        })
        .await
        .expect("the daemon reads the packet");
        assert_eq!(seen, Some(outward), "the packet arrives as sent");

        let inward = packet::udp_packet(web, SocketAddrV4::new(address, 50_000), b"hello back")
            .expect("a packet to receive");
        to_session
            .send(inward.clone())
            .expect("the daemon writes it");
        let tun = tuns
            .lock()
            .unwrap()
            .first()
            .expect("the helper opened a tunnel")
            .try_clone()
            .expect("a tunnel");
        tun.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("a timeout");
        let (read, length) = tokio::task::spawn_blocking(move || {
            let mut read = vec![0_u8; 4096];
            let length = tun.recv(&mut read).expect("the packet reaches the stack");
            (read, length)
        })
        .await
        .expect("the read ends");
        assert_eq!(read.get(..length).map(<[u8]>::to_vec), Some(inward));
    }

    /// Wait for `condition` , or fail saying what was waited for.
    async fn until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(5))
            .expect("a deadline far inside an Instant");
        while !condition() {
            assert!(std::time::Instant::now() < deadline, "waited for {what}");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// The person chooses to route their home network through another of their devices: the
    /// session asks the daemon for the tunnel, enters it by the helper's TUN with what the exit
    /// offered, carries their traffic both ways, and undoes the route when they turn it off.
    #[test]
    fn a_route_through_a_device_enters_by_the_helper_and_is_carried_by_the_daemon() {
        let _home = tilo_test_env::TestHome::new().expect("a home of its own");
        let runtime = tilo_test_env::runtime_tempdir().expect("a runtime directory");
        let socket = runtime.path().join("helper.sock");
        let tuns: Arc<Mutex<Vec<UnixDatagram>>> = Arc::default();
        let asked = helper_at(&socket, Arc::clone(&tuns));

        let address: std::net::Ipv4Addr = "100.100.64.2".parse().expect("a tunnel address");
        let offer = Offer {
            gateway: device_vpn::net::GATEWAY,
            address: Some(address),
            mtu: device_vpn::tunnel::exit::EXIT_MTU,
            home_networks: vec!["192.168.1.0/24".parse().expect("a network")],
        };
        let want = Want {
            device: [7; 32],
            routes: settings::Routes::HomeNetwork,
            dns: true,
            kill_switch: false,
        };
        let (inputs, _) = watch::channel(Inputs {
            want: Some(want),
            ..Inputs::default()
        });
        let publisher = Publisher::default();
        let (heard, mut from_session) = tokio::sync::mpsc::unbounded_channel();
        let (to_session, write) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let writes = Arc::new(tokio::sync::Mutex::new(Some(write)));
        let opening = {
            let writes = Arc::clone(&writes);
            Opening(Arc::new(move || {
                let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a pair");
                ours.set_nonblocking(true).expect("nonblocking");
                theirs.set_nonblocking(true).expect("nonblocking");
                let theirs = tokio::net::UnixStream::from_std(theirs).expect("a socket");
                let writes = Arc::clone(&writes);
                let offer = offer.clone();
                let heard = heard.clone();
                Box::pin(async move {
                    let write = writes.lock().await.take();
                    // The first ask is the route; the daemon answers it with the exit's offer.
                    let answer = Control::Offer {
                        offer,
                        mtu: device_vpn::tunnel::exit::EXIT_MTU,
                        carrier: Carrier {
                            udp_ports: vec![40_000],
                            relays: Vec::new(),
                        },
                    };
                    if let Some(write) = write {
                        tokio::spawn(daemon_side(theirs, answer, heard, write));
                    }
                    Some(tokio::net::UnixStream::from_std(ours).expect("a socket"))
                })
            }))
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let driving = publisher.clone();
        let followed = inputs.clone();
        runtime.block_on(async move {
            tokio::spawn(async move {
                drive(&socket, &opening, &driving, followed.subscribe()).await;
            });
            until("the helper to be asked for this device's tunnel", || {
                asked
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| matches!(request, Request::OpenClient { .. }))
            })
            .await;
            the_route_is_asked_for(&asked, address);
            until("Settings to show the route", || {
                let reading = publisher.0.lock_recover();
                reading
                    .client
                    .as_ref()
                    .is_some_and(|(_, state)| matches!(state, ClientState::Connected { .. }))
            })
            .await;

            // What this device's stack puts in the tunnel reaches the daemon as it went, and
            // what the exit sends comes out of this device's tunnel.
            carried(address, &tuns, &mut from_session, &to_session).await;

            // The person turns the route off: the tunnel goes and Settings stops saying it routes.
            inputs.send_replace(Inputs::default());
            until("the helper to undo this device's tunnel", || {
                asked
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| matches!(request, Request::CloseClient))
            })
            .await;
            until("Settings to show the route gone", || {
                publisher.0.lock_recover().client.is_none()
            })
            .await;
        });
    }
}
