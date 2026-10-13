//! Session-scoped GNOME facts; no other publisher's files are replaced.
use settings_store::hardware::{
    DisplayHardwareSnapshot, DisplayOutputSnapshot, LiveOutputPlacement, MonitorIdentity,
    OutputDescriptor, OutputMode, OutputTransform, logical_extent,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use zbus::zvariant::OwnedValue;

const MAX_AGE: Duration = Duration::from_millis(750);
const NAME: &str = "org.gnome.Mutter.DisplayConfig";
type Properties = HashMap<String, OwnedValue>;
type Spec = (String, String, String, String);
type Mode = (String, i32, i32, f64, f64, Vec<f64>, Properties);
type Monitor = (Spec, Vec<Mode>, Properties);
type Logical = (i32, i32, f64, u32, bool, Vec<Spec>, Properties);
type State = (u32, Vec<Monitor>, Vec<Logical>, Properties);

#[derive(Clone)]
pub(crate) struct Geometry {
    state: Arc<Mutex<Option<(DisplayHardwareSnapshot, Instant)>>>,
    cancel: CancellationToken,
}
impl Geometry {
    pub(crate) fn new(cancel: CancellationToken) -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            cancel,
        }
    }
    pub(crate) fn snapshot(&self) -> Option<DisplayHardwareSnapshot> {
        self.snapshot_at(Instant::now())
    }
    fn snapshot_at(&self, now: Instant) -> Option<DisplayHardwareSnapshot> {
        if self.cancel.is_cancelled() {
            return None;
        }
        let state = self.state.lock().ok()?;
        let (snapshot, observed) = state.as_ref()?;
        let result = (now.checked_duration_since(*observed)? < MAX_AGE).then(|| snapshot.clone());
        drop(state);
        result
    }
    fn accept(&self, snapshot: DisplayHardwareSnapshot, now: Instant) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "host geometry state unavailable")?;
        if self.cancel.is_cancelled()
            || state.as_ref().is_some_and(|(previous, observed)| {
                previous != &snapshot
                    || now
                        .checked_duration_since(*observed)
                        .is_none_or(|age| age >= MAX_AGE)
            })
        {
            *state = None;
            drop(state);
            self.cancel.cancel();
            return Err("GNOME connector geometry changed".into());
        }
        *state = Some((snapshot, now));
        drop(state);
        Ok(())
    }
    fn withdraw(&self) {
        if let Ok(mut state) = self.state.lock() {
            *state = None;
        }
        self.cancel.cancel();
    }
}
struct Withdraw(Geometry);
impl Drop for Withdraw {
    fn drop(&mut self) {
        self.0.withdraw();
    }
}

/// One bounded compositor-owner epoch, with observations withdrawn on every exit.
pub(crate) async fn observe(geometry: Geometry) -> Result<(), String> {
    let _withdraw = Withdraw(geometry.clone());
    let connection = tokio::select! {
        () = geometry.cancel.cancelled() => return Ok(()),
        result = tokio::time::timeout(MAX_AGE, zbus::Connection::session()) => result.map_err(|_| "GNOME bus setup timed out")?.map_err(|e| e.to_string())?,
    };
    observe_connection(&geometry, &connection).await
}

async fn observe_connection(
    geometry: &Geometry,
    connection: &zbus::Connection,
) -> Result<(), String> {
    let name = zbus::names::BusName::try_from(NAME).map_err(|e| e.to_string())?;
    let (bus, owner, proxy) = tokio::select! {
        () = geometry.cancel.cancelled() => return Ok(()),
        result = tokio::time::timeout(MAX_AGE, async {
            let bus = zbus::fdo::DBusProxy::new(connection).await.map_err(|e| e.to_string())?;
            let owner = bus.get_name_owner(name.clone()).await.map_err(|e| e.to_string())?;
            let proxy = zbus::Proxy::new(connection, owner.clone(), "/org/gnome/Mutter/DisplayConfig", NAME).await.map_err(|e| e.to_string())?;
            Ok::<_, String>((bus, owner, proxy))
        }) => result.map_err(|_| "GNOME owner setup timed out")??,
    };
    let mut poll = tokio::time::interval(Duration::from_millis(250));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = geometry.cancel.cancelled() => return Ok(()),
            _ = poll.tick() => {}
        }
        let read = async {
            if bus
                .get_name_owner(name.clone())
                .await
                .map_err(|e| e.to_string())?
                != owner
            {
                return Err("GNOME owner changed".to_owned());
            }
            let state: State = proxy
                .call("GetCurrentState", &())
                .await
                .map_err(|e| e.to_string())?;
            let snapshot = decode(state)?;
            if bus
                .get_name_owner(name.clone())
                .await
                .map_err(|e| e.to_string())?
                != owner
            {
                return Err("GNOME owner changed during observation".to_owned());
            }
            Ok(snapshot)
        };
        let snapshot = tokio::select! {
            () = geometry.cancel.cancelled() => return Ok(()),
            result = tokio::time::timeout(MAX_AGE, read) => result.map_err(|_| "GNOME observation timed out")??,
        };
        geometry.accept(snapshot, Instant::now())?;
    }
}

fn property_bool(properties: &Properties, name: &str) -> bool {
    properties
        .get(name)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}
fn mode(mode: &Mode) -> Result<OutputMode, String> {
    let width = u32::try_from(mode.1).map_err(|_| "invalid GNOME mode width")?;
    let height = u32::try_from(mode.2).map_err(|_| "invalid GNOME mode height")?;
    if !(1..=32768).contains(&width)
        || !(1..=32768).contains(&height)
        || !mode.3.is_finite()
        || !(1.0..=1000.0).contains(&mode.3)
    {
        return Err("invalid GNOME display mode".into());
    }
    let refresh_mhz = format!("{:.0}", mode.3 * 1000.0)
        .parse()
        .map_err(|_| "invalid GNOME refresh rate")?;
    Ok(OutputMode {
        width,
        height,
        refresh_mhz,
    })
}
fn transform(value: u32) -> Result<OutputTransform, String> {
    match value {
        0 | 4 => Ok(OutputTransform::Normal),
        1 | 5 => Ok(OutputTransform::Rotate90),
        2 | 6 => Ok(OutputTransform::Rotate180),
        3 | 7 => Ok(OutputTransform::Rotate270),
        _ => Err("invalid GNOME output transform".into()),
    }
}
fn current_mode(modes: &[Mode]) -> Result<OutputMode, String> {
    let mut current = modes.iter().filter(|m| property_bool(&m.6, "is-current"));
    let result = mode(current.next().ok_or("missing GNOME current mode")?)?;
    if current.next().is_some() {
        return Err("GNOME connector lacks one current mode".into());
    }
    Ok(result)
}

fn decode(state: State) -> Result<DisplayHardwareSnapshot, String> {
    let (_, monitors, logical, properties) = state;
    if properties
        .get("layout-mode")
        .and_then(|v| u32::try_from(v).ok())
        != Some(1)
    {
        return Err("GNOME must supply logical display coordinates".into());
    }
    if monitors.is_empty()
        || monitors.len() > settings_store::person_displays::MAX_DEVICE_OUTPUTS
        || logical.is_empty()
        || logical.len() > monitors.len()
    {
        return Err("invalid GNOME connector count".into());
    }
    let mut available = BTreeMap::new();
    for (spec, modes, props) in monitors {
        if spec.0.is_empty()
            || spec.0.len() > 256
            || modes.len() > 256
            || available
                .insert(spec.0.clone(), (spec, modes, props))
                .is_some()
        {
            return Err("ambiguous GNOME connector identity".into());
        }
    }
    let mut snapshot = DisplayHardwareSnapshot {
        backend: "gnome-display-config".into(),
        ..DisplayHardwareSnapshot::default()
    };
    let mut used = BTreeSet::new();
    for (x, y, scale, rotation, _, specs, _) in logical {
        if !scale.is_finite() || !(0.5..=4.0).contains(&scale) || specs.is_empty() {
            return Err("invalid GNOME logical display".into());
        }
        for spec in specs {
            if !used.insert(spec.0.clone()) {
                return Err("GNOME connector occupies multiple logical displays".into());
            }
            let (reported, modes, props) = available
                .get(&spec.0)
                .ok_or("GNOME logical display names an absent connector")?;
            if reported != &spec {
                return Err("GNOME connector identity changed during observation".into());
            }
            let current_mode = current_mode(modes)?;
            let (width, height) = logical_extent(
                (current_mode.width, current_mode.height),
                scale,
                transform(rotation)?,
            );
            if width == 0
                || height == 0
                || x.checked_add_unsigned(width).is_none()
                || y.checked_add_unsigned(height).is_none()
            {
                return Err("GNOME logical rectangle overflow".into());
            }
            snapshot.layout.push(LiveOutputPlacement {
                connector: spec.0.clone(),
                x,
                y,
                width,
                height,
            });
            snapshot.outputs.push(DisplayOutputSnapshot {
                descriptor: OutputDescriptor {
                    connector: spec.0.clone(),
                    connector_label: spec.0,
                    modes: modes.iter().map(mode).collect::<Result<_, _>>()?,
                    hdr: false,
                    vrr: false,
                    color_profiles: Vec::new(),
                    monitor: Some(MonitorIdentity {
                        make: spec.1,
                        model: spec.2,
                        serial: spec.3,
                        physical_size_mm: None,
                    }),
                },
                current_mode,
                scale,
                builtin: property_bool(props, "is-builtin"),
                sink_hdr: false,
                hdr_unavailable_reason: None,
                sink_vrr: false,
                vrr_unavailable_reason: None,
                color_profile_labels: BTreeMap::new(),
            });
        }
    }
    snapshot
        .layout
        .sort_by(|a, b| a.connector.cmp(&b.connector));
    snapshot
        .outputs
        .sort_by(|a, b| a.descriptor.connector.cmp(&b.descriptor.connector));
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> State {
        let spec = (
            "HDMI-1".into(),
            "Vendor".into(),
            "Panel".into(),
            "fixture".into(),
        );
        (
            1,
            vec![(
                spec.clone(),
                vec![(
                    "current".into(),
                    5120,
                    2880,
                    165.057,
                    2.0,
                    vec![1.0, 2.0],
                    HashMap::from([("is-current".into(), OwnedValue::from(true))]),
                )],
                HashMap::new(),
            )],
            vec![(1920, 0, 2.0, 0, true, vec![spec], HashMap::new())],
            HashMap::from([("layout-mode".into(), OwnedValue::from(1u32))]),
        )
    }
    #[test]
    fn actual_mode_scale_and_offset_produce_logical_geometry_without_publisher_files() {
        let d = decode(state()).expect("actual logical display shape");
        assert_eq!(
            (d.layout[0].x, d.layout[0].width, d.layout[0].height),
            (1920, 2560, 1440)
        );
        assert_eq!(d.outputs[0].current_mode.refresh_mhz, 165_057);
        assert_eq!(d.publisher_pid, 0); // In-memory facts never impersonate a leased file publisher.
    }
    #[test]
    fn stale_observations_and_cancelled_epochs_never_return_facts() {
        let cancel = CancellationToken::new();
        let g = Geometry::new(cancel.clone());
        let now = Instant::now();
        g.accept(decode(state()).expect("valid state"), now)
            .expect("initial observation");
        assert!(g.snapshot_at(now).is_some());
        assert!(
            g.snapshot_at(now.checked_add(MAX_AGE).expect("bounded deadline"))
                .is_none()
        );
        assert!(
            g.accept(
                decode(state()).expect("same facts"),
                now.checked_add(MAX_AGE).expect("deadline")
            )
            .is_err()
        );
        cancel.cancel();
        assert!(g.snapshot_at(now).is_none());
    }
    #[test]
    fn connector_layout_or_mode_changes_withdraw_the_original_epoch() {
        for change in 0..3 {
            let g = Geometry::new(CancellationToken::new());
            let now = Instant::now();
            g.accept(decode(state()).expect("initial"), now)
                .expect("initial observation");
            let mut next = state();
            match change {
                0 => next.2[0].0 = 2000,
                1 => next.2[0].2 = 1.0,
                _ => next.1[0].1[0].1 = 3840,
            }
            assert!(
                g.accept(decode(next).expect("valid changed facts"), now)
                    .is_err()
            );
            assert!(g.cancel.is_cancelled());
            assert!(g.snapshot_at(now).is_none());
        }
    }
    #[test]
    fn mixed_identity_duplicate_connector_and_nonfinite_scale_are_rejected() {
        let mut s = state();
        s.2[0].5[0].3 = "other".into();
        assert!(decode(s).is_err());
        let mut s = state();
        s.1.push((s.1[0].0.clone(), Vec::new(), HashMap::new()));
        assert!(decode(s).is_err());
        let mut s = state();
        s.2[0].2 = f64::NAN;
        assert!(decode(s).is_err());
        let mut s = state();
        s.3.clear();
        assert!(decode(s).is_err());
    }
    #[test]
    fn source_exit_cancels_capture_and_does_not_adopt_replacement_facts() {
        let g = Geometry::new(CancellationToken::new());
        g.accept(decode(state()).expect("state"), Instant::now())
            .expect("observe");
        let scope = Withdraw(g.clone());
        drop(scope);
        assert!(g.cancel.is_cancelled());
        assert!(g.snapshot().is_none());
    }
    #[test]
    fn rotation_and_invalid_rectangles_do_not_guess_portal_geometry() {
        let mut s = state();
        s.2[0].3 = 1;
        let d = decode(s).expect("portrait");
        assert_eq!((d.layout[0].width, d.layout[0].height), (1440, 2560));
        let mut s = state();
        s.2[0].0 = i32::MAX;
        assert!(decode(s).is_err());
        let mut s = state();
        s.1[0].1[0].6.clear();
        assert!(decode(s).is_err());
    }

    struct FakeBus(Arc<std::sync::atomic::AtomicBool>);
    #[zbus::interface(name = "org.freedesktop.DBus")]
    impl FakeBus {
        fn get_name_owner(&self, name: &str) -> zbus::names::UniqueName<'static> {
            assert_eq!(name, NAME);
            if self.0.load(std::sync::atomic::Ordering::SeqCst) {
                ":1.43"
            } else {
                ":1.42"
            }
            .try_into()
            .expect("fixture unique name")
        }
    }
    struct FakeDisplays(Arc<std::sync::atomic::AtomicI32>);
    #[zbus::interface(name = "org.gnome.Mutter.DisplayConfig")]
    impl FakeDisplays {
        fn get_current_state(&self) -> State {
            let mut state = state();
            state.2[0].0 = self.0.load(std::sync::atomic::Ordering::SeqCst);
            state
        }
    }
    async fn wire_change(replace_owner: bool) {
        use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
        use zbus::connection::Builder;
        let owner = Arc::new(AtomicBool::new(false));
        let position = Arc::new(AtomicI32::new(1920));
        let (server, client) = tokio::net::UnixStream::pair().expect("owned socket pair");
        let server = Builder::unix_stream(server)
            .p2p()
            .server("0123456789abcdef0123456789abcdef")
            .expect("fixture GUID")
            .serve_at("/org/freedesktop/DBus", FakeBus(Arc::clone(&owner)))
            .expect("fake bus")
            .serve_at(
                "/org/gnome/Mutter/DisplayConfig",
                FakeDisplays(Arc::clone(&position)),
            )
            .expect("fake GNOME")
            .build();
        let client = Builder::unix_stream(client).p2p().build();
        let (_server, client) = tokio::try_join!(server, client).expect("private wire handshake");
        let geometry = Geometry::new(CancellationToken::new());
        let observed = geometry.clone();
        let worker = tokio::spawn(async move {
            let _withdraw = Withdraw(observed.clone());
            observe_connection(&observed, &client).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while geometry.snapshot().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual wire observation ready");
        if replace_owner {
            owner.store(true, Ordering::SeqCst);
        } else {
            position.store(2000, Ordering::SeqCst);
        }
        let result = tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .expect("bounded withdrawal")
            .expect("observer worker");
        assert!(result.is_err());
        assert!(geometry.cancel.is_cancelled());
        assert!(geometry.snapshot().is_none());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn dbus_owner_replacement_withdraws_facts_and_cancels_capture() {
        wire_change(true).await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn dbus_connector_layout_change_withdraws_facts_and_cancels_capture() {
        wire_change(false).await;
    }
}
