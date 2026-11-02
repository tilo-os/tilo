//! The device VPN this session runs (docs/device-vpn.md): the person's VPN
//! profiles' kill switches, and this device's route through another of their
//! devices.
//!
//! Which profiles block is the device VPN crate's decision
//! ([`device_vpn::profile_block`]), followed from the person's settings and
//! `NetworkManager` and carried out by the root helper that owns the
//! firewall. Whether this device routes through another of the person's
//! devices is decided there too ([`device_vpn::controller`]): the route
//! follows the person's choice and their devices, dials the one they chose,
//! asks the helper for the tunnel, and follows what each outcome means —
//! including the route's own kill switch, which keeps traffic in while the
//! tunnel is down instead of letting it out directly.
//!
//! Both run for as long as the session does, and both are published as one
//! [`DeviceVpnStatus`] where Settings reads them: next to each profile's
//! switch the row says "blocking traffic until it is" while the block holds,
//! and the VPN list's "Through <device>" row says where the route stands and
//! what it carries. The status belongs to one session: whoever publishes it
//! says everything in it.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use device_vpn::controller::{self, AfterEnd, Want};
use device_vpn::net::{CLIENT_INTERFACE, is_relay_address};
use device_vpn::profile_block::{self, Sources, Vpns};
use device_vpn::protocol::{
    self, Carrier, ClientLink, MAX_CARRIER_PORTS, MAX_RELAYS, Request, Response,
};
use device_vpn::tunnel::client;
use device_vpn::tunnel::device::TunDevice;
use device_vpn::tunnel::{Ended, Offer};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use settings_store::device_vpn::{
    self as settings, ClientState, ClientStatus, DeviceVpnStatus, OfferedExit, Problem,
    ProfileBlockStatus, Through,
};
use settings_store::live::LiveSettings;
use tilo_device_fabric::link::SharedLink;
use tilo_lock::MutexRecover as _;
use tokio::sync::watch;

/// How long a request to the helper may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the person's online devices are asked what they offer.
const PROBE_EVERY: Duration = Duration::from_secs(300);
/// How long the relays' names may take to resolve before the route opens
/// without exempting them: opening the tunnel never waits on a lookup.
const RELAY_LOOKUP: Duration = Duration::from_secs(2);

/// The person's VPN profile kill switches and their device route, as this
/// session runs them.
pub struct Vpn {
    /// The helper's socket.
    helper: PathBuf,
    /// Where `NetworkManager`'s VPNs come from.
    vpns: Vpns,
}

impl Vpn {
    /// The machine's own: the installed helper and its `NetworkManager`.
    #[must_use]
    pub fn machine() -> Self {
        Self::with(Sources::system())
    }

    /// Against `sources`: a session that is not on the machine the helper
    /// runs on follows what it is given.
    #[must_use]
    pub fn with(sources: Sources) -> Self {
        Self {
            helper: sources.helper,
            vpns: sources.vpns,
        }
    }

    /// Run for the rest of the session: the profile kill switches from now
    /// on, and the routes, which follow the person's devices as the session
    /// passes ([`Routes::follow`]).
    pub(crate) fn start(self) -> Routes {
        let live = LiveSettings::start(|| {});
        let publisher = Publisher::default();
        // Published once before anything blocks, so Settings can tell that
        // this session runs even while nothing is blocked and nothing
        // routes.
        publisher.set_profile_block(ProfileBlockStatus::default());
        let blocking = publisher.clone();
        profile_block::start_with(
            &live,
            move |block| blocking.set_profile_block(block),
            Sources {
                helper: self.helper.clone(),
                vpns: self.vpns,
            },
        );
        Routes {
            helper: self.helper,
            live,
            publisher,
            inputs: watch::Sender::new(Inputs::default()),
            started: std::cell::Cell::new(false),
            follow: std::cell::RefCell::new(Follow::default()),
        }
    }
}

/// The session's side of its route through another of the person's devices:
/// what the person chose, which of their devices could carry it, and the
/// task that carries it out.
pub(crate) struct Routes {
    /// The helper's socket.
    helper: PathBuf,
    /// The settings the person's choices are read from.
    live: LiveSettings,
    /// What this session published last, and the file it wrote.
    publisher: Publisher,
    /// What the route's task follows.
    inputs: watch::Sender<Inputs>,
    /// Whether that task runs: it starts with the first pass that has this
    /// device's endpoint and its fabric.
    started: std::cell::Cell<bool>,
    /// What the session's passes remember between them.
    follow: std::cell::RefCell<Follow>,
}

impl Routes {
    /// Follow the person's choice and their devices for this pass: hand the
    /// route's task what changed, and publish what it needs said about them.
    pub(crate) fn follow(&self, fabric: Option<&SharedLink>, endpoint: Option<Endpoint>) {
        let (Some(fabric), Some(endpoint)) = (fabric, endpoint) else {
            return;
        };
        let mut follow = self.follow.borrow_mut();
        let generation = self.live.generation();
        if follow.settings_generation != Some(generation) {
            follow.settings_generation = Some(generation);
            let section = self.live.section(settings::SECTION);
            follow.exit_on = settings::EXIT.from_section(&section);
            follow.through = settings::THROUGH.from_section(&section);
        }
        self.publisher.set_exit_on(follow.exit_on);
        if !self.started.get() {
            self.started.set(true);
            self.start(endpoint);
        }

        // The person's devices as their fabric names them: whose name to
        // show beside the route, and which of them could carry it.
        let devices = fabric.device_fabric();
        let mut labels = BTreeMap::new();
        let mut online = BTreeSet::new();
        for snapshot in devices.snapshots().values() {
            let Some(key) = key_of(&snapshot.owner) else {
                continue;
            };
            labels.insert(key, snapshot.presence.label.clone());
            if snapshot.presence.online {
                online.insert(key);
            }
        }
        self.publisher.set_labels(labels);
        let want = follow.through.as_ref().and_then(Want::of);
        self.inputs.send_if_modified(|inputs| {
            let changed = inputs.want != want || inputs.online != online;
            if changed {
                *inputs = Inputs { want, online };
            }
            changed
        });
    }

    /// Start the task that dials and carries the route, for the rest of the
    /// session.
    fn start(&self, endpoint: Endpoint) {
        let helper = self.helper.clone();
        let publisher = self.publisher.clone();
        let inputs = self.inputs.subscribe();
        let probing = inputs.clone();
        let started = std::thread::Builder::new()
            .name("tilo-device-vpn-route".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(async move {
                        tokio::join!(
                            drive(&helper, &endpoint, &publisher, inputs),
                            probe_exits(&endpoint, &publisher, probing),
                        );
                    }),
                    Err(error) => {
                        tracing::warn!(%error, "could not start the route through a device");
                    }
                }
            });
        if let Err(error) = started {
            tracing::warn!(%error, "could not start the route through a device");
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
}

/// What the route's task follows: the person's choice, and their devices
/// with a session right now.
#[derive(Clone, Default)]
struct Inputs {
    want: Option<Want>,
    online: BTreeSet<[u8; 32]>,
}

/// The key a device is dialled by, from the id its fabric knows it by.
fn key_of(device: &tilo_device_fabric::DeviceId) -> Option<[u8; 32]> {
    let mut key = [0_u8; 32];
    hex::decode_to_slice(device.as_str().trim(), &mut key).ok()?;
    Some(key)
}

/// What this session published of its device VPN, and the file it wrote.
///
/// The kill switches and the route report from their own threads; every part
/// is written out as one [`DeviceVpnStatus`], since Settings reads one file
/// and a part written alone would say the rest is off. The first report of
/// any part writes, even when the status is all defaults: Settings reads it
/// to know this session runs.
#[derive(Clone, Default)]
struct Publisher(Arc<Mutex<Published>>);

#[derive(Default)]
struct Published {
    profile_block: ProfileBlockStatus,
    exit_on: bool,
    labels: BTreeMap<[u8; 32], String>,
    offered: Vec<[u8; 32]>,
    client: Option<(Want, ClientState)>,
    /// This session's lease on the status file, and the status as last
    /// written.
    lease: Option<(u32, u64)>,
    written: Option<DeviceVpnStatus>,
}

impl Publisher {
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
            // This session serves no exit: nothing here answers another
            // device asking to route through this one, so Settings is told
            // it cannot rather than offered a switch that does nothing.
            exit_available: false,
            exit_on: self.exit_on,
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
    tokio::task::spawn_blocking(move || protocol::call(&helper, &request, HELPER_TIMEOUT))
        .await
        .map_err(|_| Problem::Setup)?
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                Problem::NotInstalled
            }
            _ => Problem::Setup,
        })
}

fn unix_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

/// The client's link while the helper holds it.
struct Held {
    want: Want,
    link: ClientLink,
    device: Arc<TunDevice>,
}

fn set_state(publisher: &Publisher, want: &Want, state: ClientState) {
    publisher.set_client(Some((want.clone(), state)));
}

/// Resolves once the person's choice is no longer `want`. Never, once nobody
/// can change it.
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

/// The relays' addresses, kept from the last lookup that found any: a lookup
/// while the kill switch blocks finds none.
static RELAYS: Mutex<Vec<IpAddr>> = Mutex::new(Vec::new());

async fn relay_addresses() -> Vec<IpAddr> {
    let urls: Vec<iroh::RelayUrl> = iroh::endpoint::default_relay_mode().relay_map().urls();
    let mut found = tokio::time::timeout(RELAY_LOOKUP, async move {
        let mut found = Vec::new();
        for url in urls {
            let Some(host) = url.host_str() else {
                continue;
            };
            if let Ok(addresses) = tokio::net::lookup_host((host, 443)).await {
                found.extend(
                    addresses
                        .map(|address| address.ip())
                        .filter(|ip| is_relay_address(*ip)),
                );
            }
        }
        found
    })
    .await
    .unwrap_or_default();
    found.sort_unstable();
    found.dedup();
    found.truncate(MAX_RELAYS);
    let mut kept = RELAYS.lock_recover();
    if !found.is_empty() {
        kept.clone_from(&found);
    }
    kept.clone()
}

/// What must reach the exit outside the tunnel: this endpoint's sockets and
/// the relays.
async fn carrier(endpoint: &Endpoint) -> Carrier {
    let mut udp_ports: Vec<u16> = endpoint
        .bound_sockets()
        .iter()
        .map(SocketAddr::port)
        .filter(|port| *port != 0)
        .collect();
    udp_ports.sort_unstable();
    udp_ports.dedup();
    udp_ports.truncate(MAX_CARRIER_PORTS);
    Carrier {
        udp_ports,
        relays: relay_addresses().await,
    }
}

/// Clear the person's choice of `want`'s device: that device stopped this
/// one.
fn clear_through(want: &Want) {
    let device = hex::encode(want.device);
    let submitted = tilo_durable_writer::global().submit_ordered(move || {
        let cleared = settings::THROUGH.update(|through| through.filter(|t| t.device != device));
        if let Err(error) = cleared {
            tracing::warn!(%error, "could not clear the device this one routes through");
        }
    });
    if let Err(error) = submitted {
        tracing::warn!(%error, "could not queue clearing the device this one routes through");
    }
}

/// Do what `after` says, then wait as long as it says or until the choice
/// changes.
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
        clear_through(want);
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

/// The helper's link for `offer`: the one held when it still fits, else a
/// new one. On failure, what to do instead.
async fn link_up(
    helper: &Path,
    endpoint: &Endpoint,
    want: &Want,
    offer: &Offer,
    address: std::net::Ipv4Addr,
    mtu: u16,
    held: &mut Option<Held>,
    attempt: u32,
) -> Result<Arc<TunDevice>, AfterEnd> {
    let keep_link = held.is_some() && want.kill_switch;
    let carrier = carrier(endpoint).await;
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

/// Route through the device the person chose, for as long as they choose it.
async fn drive(
    helper: &Path,
    endpoint: &Endpoint,
    publisher: &Publisher,
    mut inputs: watch::Receiver<Inputs>,
) {
    let mut held: Option<Held> = None;
    let mut attempt: u32 = 0;
    // A route a crashed session left with its kill switch: undone unless the
    // person still wants it, when the new link replaces it.
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
        let Ok(exit) = EndpointId::from_bytes(&want.device) else {
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
        let dialled = tokio::select! {
            result = client::connect(endpoint, EndpointAddr::new(exit)) => Some(result),
            () = choice_changes(&mut inputs, &want) => None,
        };
        let Some(dialled) = dialled else { continue };
        let tunnel = match dialled {
            Ok(tunnel) => tunnel,
            Err(error) => {
                let after = controller::after_failed_dial(
                    &error,
                    held.is_some(),
                    want.kill_switch,
                    attempt,
                );
                settle(helper, publisher, &mut held, &want, after, &mut inputs).await;
                continue;
            }
        };
        let device = match link_up(
            helper,
            endpoint,
            &want,
            &tunnel.offer,
            tunnel.address,
            tunnel.mtu(),
            &mut held,
            attempt,
        )
        .await
        {
            Ok(device) => device,
            Err(after) => {
                tunnel.close();
                settle(helper, publisher, &mut held, &want, after, &mut inputs).await;
                continue;
            }
        };
        attempt = 0;
        set_state(
            publisher,
            &want,
            ClientState::Connected {
                since_unix_ms: unix_ms(SystemTime::now()),
            },
        );
        let ended = tokio::select! {
            ended = client::run(&tunnel, device) => ended,
            () = choice_changes(&mut inputs, &want) => {
                tunnel.close();
                Ended::Closed
            }
        };
        tracing::info!(?ended, "the route through another device ended");
        let after = controller::after_end(&ended, want.kill_switch, 1);
        settle(helper, publisher, &mut held, &want, after, &mut inputs).await;
    }
}

/// Learn which of the person's online devices offer themselves: when the
/// online devices change, and every [`PROBE_EVERY`].
async fn probe_exits(
    endpoint: &Endpoint,
    publisher: &Publisher,
    mut inputs: watch::Receiver<Inputs>,
) {
    let mut asked: Option<(BTreeSet<[u8; 32]>, Instant)> = None;
    loop {
        let current = inputs.borrow_and_update().clone();
        let due = asked
            .as_ref()
            .is_none_or(|(online, at)| *online != current.online || at.elapsed() >= PROBE_EVERY);
        if due {
            let mut offered = Vec::new();
            for device in &current.online {
                let Ok(id) = EndpointId::from_bytes(device) else {
                    continue;
                };
                if client::probe(endpoint, EndpointAddr::new(id)).await.is_ok() {
                    offered.push(*device);
                }
            }
            publisher.set_offered(offered);
            asked = Some((current.online.clone(), Instant::now()));
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
