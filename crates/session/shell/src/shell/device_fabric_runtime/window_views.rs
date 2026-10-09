//! Which of the person's devices show which legacy app windows.
//!
//! A legacy app (a Wayland or X11 client) runs on one device only, the one
//! that started it; its tab reaches the person's other devices as a
//! `HostedWindow` naming that device. Every other device that shows the tab
//! gets a stream of just that window from there. A viewing device tells each
//! host which of its windows it shows and at what size, as a
//! [`PersonalFabricAction::WindowViews`], and repeats it while it shows them;
//! the host streams a listed window while the list keeps arriving and still
//! names it.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::device_fabric::{HardwareDecode, PersonalFabricAction, WindowView};
use crate::shell::network::ShellNetwork;
use crate::shell::personal_continuity::DeviceId;

/// How often an unchanged, non-empty list is repeated.
const REFRESH: Duration = Duration::from_secs(2);
/// A viewer whose list has not been repeated for this long has stopped
/// showing its windows: it closed them, slept, or lost the network.
const EXPIRY: Duration = Duration::from_secs(6);

/// One device showing a window this device hosts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowViewer {
    pub device: DeviceId,
    /// The viewer's peer handle on the workspace topic, the address its
    /// stream and its input travel under.
    pub peer: u64,
    pub width: u32,
    pub height: u32,
    /// The largest picture the viewer's hardware decoders take, by codec.
    pub decode_limits: BTreeMap<media::StreamCodec, (u32, u32)>,
}

struct Viewing {
    views: Vec<WindowView>,
    decode_limits: BTreeMap<media::StreamCodec, (u32, u32)>,
    at: Instant,
}

/// The limits a viewer reported, by codec; codecs this build does not know
/// and codecs without a limit carry none.
fn limits_of(decode: &[HardwareDecode]) -> BTreeMap<media::StreamCodec, (u32, u32)> {
    decode
        .iter()
        .filter_map(|entry| {
            Some((
                media::StreamCodec::from_wire_label(&entry.codec)?,
                entry.max?,
            ))
        })
        .collect()
}

/// What this device's hardware decoders take, as `WindowViews` reports it.
fn local_decode() -> Vec<HardwareDecode> {
    let caps = media::realtime::local_capabilities();
    caps.hardware_decode
        .iter()
        .map(|codec| HardwareDecode {
            codec: codec.wire_label().to_owned(),
            max: caps.hardware_decode_max.get(codec).copied(),
        })
        .collect()
}

#[derive(Default)]
struct Inbound {
    devices: BTreeMap<DeviceId, Viewing>,
}

impl Inbound {
    fn record(
        &mut self,
        source: DeviceId,
        views: Vec<WindowView>,
        decode: &[HardwareDecode],
        now: Instant,
    ) {
        if views.is_empty() {
            self.devices.remove(&source);
        } else {
            self.devices.insert(
                source,
                Viewing {
                    views,
                    decode_limits: limits_of(decode),
                    at: now,
                },
            );
        }
    }

    fn viewers(&mut self, now: Instant) -> BTreeMap<u64, Vec<WindowViewer>> {
        self.devices
            .retain(|_, viewing| now.saturating_duration_since(viewing.at) < EXPIRY);
        let mut viewers: BTreeMap<u64, Vec<WindowViewer>> = BTreeMap::new();
        for (device, viewing) in &self.devices {
            let Some(peer) = peer_handle(device) else {
                continue;
            };
            for view in &viewing.views {
                viewers.entry(view.tab).or_default().push(WindowViewer {
                    device: device.clone(),
                    peer,
                    width: view.width,
                    height: view.height,
                    decode_limits: viewing.decode_limits.clone(),
                });
            }
        }
        viewers
    }
}

struct Told {
    views: Vec<WindowView>,
    at: Instant,
}

/// What each host was last told.
#[derive(Default)]
struct Outbound {
    hosts: BTreeMap<DeviceId, Told>,
}

impl Outbound {
    /// The lists to send now, by host: on a change of what is shown of its
    /// windows, and as a refresh while any is. A host none of whose windows
    /// is shown any more hears the empty list once, so its streams end at
    /// once rather than at expiry.
    fn due(
        &mut self,
        shown: BTreeMap<DeviceId, Vec<WindowView>>,
        now: Instant,
    ) -> Vec<(DeviceId, Vec<WindowView>)> {
        let mut out = self
            .hosts
            .keys()
            .filter(|host| !shown.contains_key(*host))
            .map(|host| (host.clone(), Vec::new()))
            .collect::<Vec<_>>();
        self.hosts.retain(|host, _| shown.contains_key(host));
        for (host, views) in shown {
            let due = self.hosts.get(&host).is_none_or(|told| {
                told.views != views || now.saturating_duration_since(told.at) >= REFRESH
            });
            if due {
                out.push((host.clone(), views.clone()));
                self.hosts.insert(host, Told { views, at: now });
            }
        }
        out
    }
}

fn inbound() -> &'static Mutex<Inbound> {
    static STATE: OnceLock<Mutex<Inbound>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(Inbound::default()))
}

fn outbound() -> &'static Mutex<Outbound> {
    static STATE: OnceLock<Mutex<Outbound>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(Outbound::default()))
}

/// The workspace-topic peer handle of a certified device: devices are
/// addressed on the fabric by their endpoint key.
pub(crate) fn peer_handle(device: &DeviceId) -> Option<u64> {
    let bytes: [u8; 32] = hex::decode(device.as_str()).ok()?.try_into().ok()?;
    let endpoint = iroh::EndpointId::from_bytes(&bytes).ok()?;
    Some(crate::network::behaviour::peer_liveness_handle(&endpoint))
}

/// Take what `source` says it shows now.
pub(super) fn record(source: DeviceId, views: Vec<WindowView>, decode: &[HardwareDecode]) {
    if let Ok(mut state) = inbound().lock() {
        state.record(source, views, decode, Instant::now());
    }
}

/// The person's other devices showing each tab, by tab, while app
/// continuity is on; this device streams the windows it hosts among them.
pub fn window_viewers() -> BTreeMap<u64, Vec<WindowViewer>> {
    if !ContinuityPolicySnapshot::load().permits(ContinuityCapability::AppContinuations) {
        if let Ok(mut state) = inbound().lock() {
            state.devices.clear();
        }
        return BTreeMap::new();
    }
    inbound().lock().map_or_else(
        |_| BTreeMap::new(),
        |mut state| state.viewers(Instant::now()),
    )
}

/// Tell each host which of its legacy windows this device shows, given as
/// (host device key, window) pairs; cheap when nothing changed, so call it
/// every tick. Only the person's own online devices are told.
pub fn publish_window_views(network: &ShellNetwork, shown: Vec<(String, WindowView)>) {
    let shown = if ContinuityPolicySnapshot::load().permits(ContinuityCapability::AppContinuations)
    {
        by_online_host(network, shown)
    } else {
        BTreeMap::new()
    };
    let due = outbound()
        .lock()
        .map_or_else(|_| Vec::new(), |mut state| state.due(shown, Instant::now()));
    if due.is_empty() {
        return;
    }
    let decode = local_decode();
    for (host, views) in due {
        let action = PersonalFabricAction::WindowViews {
            views,
            decode: decode.clone(),
        };
        if let Err(error) = network.send_personal_fabric_action(&host, action) {
            tracing::debug!(%error, "window views not sent");
        }
    }
}

/// `shown` grouped by host, keeping only the person's other devices that are
/// online, each list in tab order with one size per tab and within the
/// wire's bound.
fn by_online_host(
    network: &ShellNetwork,
    shown: Vec<(String, WindowView)>,
) -> BTreeMap<DeviceId, Vec<WindowView>> {
    let Some((local, _)) = network.local_device_fabric_identity() else {
        return BTreeMap::new();
    };
    let mut online = network.online_device_ids();
    online.remove(&local);
    let mut hosts: BTreeMap<DeviceId, Vec<WindowView>> = BTreeMap::new();
    for (host, view) in shown {
        let Ok(host) = DeviceId::new(host) else {
            continue;
        };
        if online.contains(&host) {
            hosts.entry(host).or_default().push(view);
        }
    }
    for views in hosts.values_mut() {
        views.sort_by_key(|view| view.tab);
        views.dedup_by_key(|view| view.tab);
        views.truncate(crate::shell::device_fabric::MAX_WINDOW_VIEWS);
    }
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(byte: u8) -> DeviceId {
        DeviceId::new(hex::encode(
            iroh::SecretKey::from_bytes(&[byte; 32]).public().as_bytes(),
        ))
        .unwrap()
    }

    fn view(tab: u64) -> WindowView {
        WindowView {
            tab,
            width: 800,
            height: 600,
        }
    }

    #[test]
    fn viewers_follow_the_latest_list_and_lapse_without_refresh() {
        let mut inbound = Inbound::default();
        let start = Instant::now();
        let laptop = device(1);
        let phone = device(2);
        inbound.record(
            laptop.clone(),
            vec![view(7), view(9)],
            &[HardwareDecode {
                codec: "h264".into(),
                max: Some((1920, 1088)),
            }],
            start,
        );
        inbound.record(phone.clone(), vec![view(7)], &[], start);
        let viewers = inbound.viewers(start);
        assert_eq!(viewers[&7].len(), 2);
        assert_eq!(viewers[&9].len(), 1);
        assert_eq!(viewers[&9][0].peer, peer_handle(&laptop).unwrap());
        assert_eq!(
            viewers[&9][0].decode_limits,
            BTreeMap::from([(media::StreamCodec::H264, (1920, 1088))]),
            "the viewer's decoder limit reaches the host"
        );

        // A newer list replaces the older one entirely.
        inbound.record(laptop, vec![view(9)], &[], start);
        let viewers = inbound.viewers(start);
        assert_eq!(viewers[&7].len(), 1);
        assert_eq!(viewers[&7][0].device, phone);

        // An empty list ends that device's views at once.
        inbound.record(phone, Vec::new(), &[], start);
        assert!(!inbound.viewers(start).contains_key(&7));

        // Silence ends them at expiry.
        assert!(inbound.viewers(start + EXPIRY / 2).contains_key(&9));
        assert!(inbound.viewers(start + EXPIRY).is_empty());
    }

    #[test]
    fn each_host_hears_its_own_windows_refreshed_and_one_final_empty_list() {
        let mut outbound = Outbound::default();
        let start = Instant::now();
        let desk = device(3);
        let tablet = device(4);
        let shown = |entries: &[(&DeviceId, u64)]| {
            let mut hosts: BTreeMap<DeviceId, Vec<WindowView>> = BTreeMap::new();
            for (host, tab) in entries {
                hosts.entry((*host).clone()).or_default().push(view(*tab));
            }
            hosts
        };

        // Nothing shown, nothing to say.
        assert!(outbound.due(BTreeMap::new(), start).is_empty());

        let sent = outbound.due(shown(&[(&desk, 5), (&tablet, 6)]), start);
        assert_eq!(
            sent.into_iter().collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                (desk.clone(), vec![view(5)]),
                (tablet.clone(), vec![view(6)])
            ])
        );
        assert!(
            outbound
                .due(shown(&[(&desk, 5), (&tablet, 6)]), start)
                .is_empty()
        );

        // Only the host whose windows changed hears about it.
        let sent = outbound.due(shown(&[(&desk, 5), (&desk, 7), (&tablet, 6)]), start);
        assert_eq!(sent, vec![(desk.clone(), vec![view(5), view(7)])]);

        // Refreshes keep the streams alive.
        let later = start + REFRESH;
        assert_eq!(
            outbound
                .due(shown(&[(&desk, 5), (&desk, 7), (&tablet, 6)]), later)
                .len(),
            2
        );

        // A host none of whose windows is shown hears the empty list once.
        let sent = outbound.due(shown(&[(&desk, 5), (&desk, 7)]), later);
        assert_eq!(sent, vec![(tablet, Vec::new())]);
        assert!(
            outbound
                .due(shown(&[(&desk, 5), (&desk, 7)]), later)
                .is_empty()
        );
        assert_eq!(
            outbound.due(BTreeMap::new(), later),
            vec![(desk, Vec::new())]
        );
        assert!(outbound.due(BTreeMap::new(), later + REFRESH).is_empty());
    }
}
