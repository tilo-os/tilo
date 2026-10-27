//! Give every kiosk output its panel-DPI scale and keep the greeter told
//! where each output is. This speaks the compositor's standard
//! output-management protocol; it never changes mode, transform or power.
//!
//! `cage` lays its outputs side by side and stretches the one greeter window
//! over the whole layout. Two things follow:
//!
//! - Changing one head's scale changes its logical width, so the positions
//!   the kiosk chose for the old widths would leave a gap (or an overlap)
//!   beside it. Every configuration therefore packs the enabled heads again,
//!   left to right in their existing order, at their new logical widths.
//! - The window spans every output, so a centred sign-in card lands on the
//!   seam between two displays, or in a gap where no display shows it. The
//!   greeter reads [`Displays::arrangement`] and places the card on the
//!   primary output; the backdrop covers each output.
//!
//! A connection stays open for the greeter's lifetime: a hotplugged display
//! is configured like one present at startup, a configuration the kiosk
//! cancels because its outputs changed meanwhile is planned again from the
//! new description, and a rejected one leaves the greeter on the geometry
//! the kiosk reports, never on a guessed one.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tilo_lock::MutexRecover as _;
use vello::kurbo::Rect;
use wayland_client::protocol::{wl_callback, wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

/// Startup waits at most this long for the first configuration to settle
/// before winit observes the window's scale; the watcher carries on after.
const STARTUP_LIMIT: Duration = Duration::from_secs(2);

/// The external-event payload that tells the greeter the outputs changed.
pub const OUTPUTS_WAKE: &str = "\u{1}greeter-outputs";

/// One enabled output, in the kiosk's layout coordinates (logical pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    pub rect: Rect,
    /// A built-in panel (eDP, LVDS, DSI): where the card goes when present.
    pub internal: bool,
}

/// Every enabled output, ordered left to right.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arrangement {
    pub displays: Vec<Display>,
}

impl Arrangement {
    /// The layout's bounding box: what `cage` stretches the window over.
    #[must_use]
    pub fn extent(&self) -> Option<Rect> {
        self.displays
            .iter()
            .map(|display| display.rect)
            .reduce(|a, b| a.union(b))
    }

    /// Each output relative to the window's origin, the layout's top-left.
    #[must_use]
    pub fn in_window(&self) -> Vec<Rect> {
        let Some(extent) = self.extent() else {
            return Vec::new();
        };
        self.displays
            .iter()
            .map(|display| {
                Rect::from_origin_size(
                    (display.rect.x0 - extent.x0, display.rect.y0 - extent.y0),
                    display.rect.size(),
                )
            })
            .collect()
    }

    /// The output that carries the sign-in card, relative to the window:
    /// the built-in panel when there is one, else the leftmost output.
    #[must_use]
    pub fn primary_in_window(&self) -> Option<Rect> {
        let index = self
            .displays
            .iter()
            .position(|display| display.internal)
            .unwrap_or(0);
        self.in_window().get(index).copied()
    }
}

/// The shared view of the kiosk's outputs, kept current by the watcher.
#[derive(Clone, Default)]
pub struct Displays {
    arrangement: Arc<Mutex<Arrangement>>,
    wake: Arc<OnceLock<Box<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for Displays {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Displays")
            .field("arrangement", &self.arrangement())
            .finish_non_exhaustive()
    }
}

impl Displays {
    /// A fixed arrangement, for renders and tests without a compositor.
    #[must_use]
    pub fn fixed(arrangement: Arrangement) -> Self {
        Self {
            arrangement: Arc::new(Mutex::new(arrangement)),
            wake: Arc::default(),
        }
    }

    /// The outputs as the kiosk last described them.
    #[must_use]
    pub fn arrangement(&self) -> Arrangement {
        self.arrangement.lock_recover().clone()
    }

    /// Called whenever the arrangement changes, once the UI can be woken.
    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.wake.set(Box::new(wake));
    }

    fn publish(&self, arrangement: Arrangement) {
        let changed = {
            let mut current = self.arrangement.lock_recover();
            let changed = *current != arrangement;
            *current = arrangement;
            changed
        };
        if changed && let Some(wake) = self.wake.get() {
            wake();
        }
    }
}

/// What the kiosk reports about one head, free of protocol objects.
#[derive(Clone, Debug, Default, PartialEq)]
struct Facts {
    name: Option<String>,
    enabled: Option<bool>,
    position: Option<(i32, i32)>,
    transform: Option<wl_output::Transform>,
    scale: Option<f64>,
    physical_size: Option<(u32, u32)>,
    mode_size: Option<(u32, u32)>,
    adaptive_sync_invalid: bool,
}

/// Where and at which scale one enabled head should be.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Target {
    position: (i32, i32),
    scale: f64,
}

impl Facts {
    fn internal(&self) -> bool {
        self.name.as_deref().is_some_and(|name| {
            ["eDP", "LVDS", "DSI"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
    }

    fn desired_scale(&self) -> Option<f64> {
        let current = self.scale?;
        // An explicit kiosk scale is authoritative. Only repair the
        // unconfigured 1x case; unknown panel geometry keeps 1x.
        if current.to_bits() != 1.0_f64.to_bits() {
            return Some(current);
        }
        // Match the native desktop's panel policy on the raw mode geometry;
        // output transforms change placement, not the panel's measured DPI.
        Some(settings_store::display_scale::derive_scale(
            self.mode_size?,
            self.physical_size,
        ))
    }

    /// The head's size in layout coordinates at `scale`, as wlroots derives
    /// it: the transformed mode divided by the scale, truncated.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "wlroots truncates the effective resolution the same way; a mode is far below i32::MAX"
    )]
    fn logical_size(&self, scale: f64) -> Option<(i32, i32)> {
        let (width, height) = self.mode_size?;
        let (width, height) = match self.transform? {
            wl_output::Transform::_90
            | wl_output::Transform::_270
            | wl_output::Transform::Flipped90
            | wl_output::Transform::Flipped270 => (height, width),
            _ => (width, height),
        };
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        Some((
            (f64::from(width) / scale) as i32,
            (f64::from(height) / scale) as i32,
        ))
    }
}

/// `scale` as the protocol carries it (`wl_fixed`, 1/256 steps), so a
/// planned scale compares equal to the one the kiosk reports back.
fn quantized(scale: f64) -> f64 {
    (scale * 256.0).round() / 256.0
}

/// Enabled heads, left to right (then top to bottom), as indices.
fn layout_order(heads: &[Facts]) -> Vec<(usize, &Facts)> {
    let mut order: Vec<(usize, &Facts)> = heads
        .iter()
        .enumerate()
        .filter(|(_, head)| head.enabled == Some(true))
        .collect();
    order.sort_by_key(|&(index, head)| {
        let (x, y) = head.position.unwrap_or_default();
        (x, y, index)
    });
    order
}

/// The configuration every head should have, or why the description cannot
/// be configured as a whole. Disabled heads have no target.
fn plan(heads: &[Facts]) -> Result<Vec<Option<Target>>, &'static str> {
    let mut targets = vec![None; heads.len()];
    for head in heads {
        match head.enabled {
            Some(false) => (),
            Some(true) if head.adaptive_sync_invalid => {
                return Err("output adaptive sync state unknown");
            }
            Some(true) if head.position.is_some() => (),
            _ => return Err("output description incomplete"),
        }
    }
    let mut x = 0_i32;
    for (index, head) in layout_order(heads) {
        let scale = quantized(
            head.desired_scale()
                .ok_or("output description incomplete")?,
        );
        let (width, _) = head
            .logical_size(scale)
            .ok_or("output description incomplete")?;
        if let Some(slot) = targets.get_mut(index) {
            *slot = Some(Target {
                position: (x, 0),
                scale,
            });
        }
        x = x.saturating_add(width);
    }
    Ok(targets)
}

/// Whether applying `targets` would change anything the kiosk reports.
fn differs(heads: &[Facts], targets: &[Option<Target>]) -> bool {
    heads.iter().zip(targets).any(|(head, target)| {
        target.is_some_and(|target| {
            head.position != Some(target.position)
                || head.scale.map(|scale| quantized(scale).to_bits())
                    != Some(target.scale.to_bits())
        })
    })
}

/// The outputs as currently configured, for the greeter's layout.
fn arrangement(heads: &[Facts]) -> Arrangement {
    let displays = layout_order(heads)
        .into_iter()
        .filter_map(|(_, head)| {
            let (x, y) = head.position?;
            let (width, height) = head.logical_size(head.scale?)?;
            Some(Display {
                rect: Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x) + f64::from(width),
                    f64::from(y) + f64::from(height),
                ),
                internal: head.internal(),
            })
        })
        .collect();
    Arrangement { displays }
}

struct Head {
    object: ZwlrOutputHeadV1,
    facts: Facts,
    current_mode: Option<ZwlrOutputModeV1>,
    adaptive_sync: Option<zwlr_output_head_v1::AdaptiveSyncState>,
}

struct Modes {
    mode: ZwlrOutputModeV1,
    size: Option<(u32, u32)>,
}

struct Outputs {
    displays: Displays,
    manager: Option<ZwlrOutputManagerV1>,
    heads: Vec<Head>,
    modes: Vec<Modes>,
    serial: Option<u32>,
    registry_done: bool,
    /// The configuration awaiting the kiosk's answer, with its serial.
    pending: Option<(ZwlrOutputConfigurationV1, u32)>,
    /// The serial whose configuration the kiosk refused: not retried until
    /// the outputs change.
    rejected: Option<u32>,
    /// The last plan the kiosk accepted. Re-planning the same targets after
    /// the kiosk adjusted them means it decided otherwise: it is not
    /// requested again, so a compositor's own policy can never loop with ours.
    accepted: Option<Vec<Option<Target>>>,
    /// The plan awaiting the kiosk's answer.
    requested: Option<Vec<Option<Target>>>,
    /// Startup may proceed: nothing (more) to configure.
    settled: bool,
}

impl Outputs {
    fn new(displays: Displays) -> Self {
        Self {
            displays,
            manager: None,
            heads: Vec::new(),
            modes: Vec::new(),
            serial: None,
            registry_done: false,
            pending: None,
            rejected: None,
            accepted: None,
            requested: None,
            settled: false,
        }
    }

    fn facts(&self) -> Vec<Facts> {
        self.heads
            .iter()
            .map(|head| Facts {
                mode_size: head.current_mode.as_ref().and_then(|mode| {
                    self.modes
                        .iter()
                        .find(|known| known.mode == *mode)
                        .and_then(|known| known.size)
                }),
                ..head.facts.clone()
            })
            .collect()
    }

    /// The kiosk finished describing its outputs: publish them, and
    /// configure them when they differ from the plan.
    fn reconsider(&mut self, qh: &QueueHandle<Self>) {
        let Some(serial) = self.serial else {
            return;
        };
        let heads = self.facts();
        self.displays.publish(arrangement(&heads));
        if self.pending.is_some() || self.rejected == Some(serial) {
            return;
        }
        let targets = match plan(&heads) {
            Ok(targets) => targets,
            Err(error) => {
                tracing::warn!(%error, "greeter outputs keep the kiosk's configuration");
                self.settled = true;
                return;
            }
        };
        if !differs(&heads, &targets) || self.accepted.as_ref() == Some(&targets) {
            self.settled = true;
            return;
        }
        let Some(manager) = &self.manager else {
            return;
        };
        let config = manager.create_configuration(serial, qh, ());
        for (head, target) in self.heads.iter().zip(&targets) {
            let Some(target) = target else {
                config.disable_head(&head.object);
                continue;
            };
            let enabled = config.enable_head(&head.object, qh, ());
            // Planned heads have a current mode and transform.
            if let Some(mode) = &head.current_mode {
                enabled.set_mode(mode);
            }
            if let Some(transform) = head.facts.transform {
                enabled.set_transform(transform);
            }
            enabled.set_position(target.position.0, target.position.1);
            enabled.set_scale(target.scale);
            // A non-VRR head may omit this event even at version 4. Keep
            // its setting untouched; only copy an actually advertised value.
            if head.object.version() >= 4
                && let Some(adaptive_sync) = head.adaptive_sync
            {
                enabled.set_adaptive_sync(adaptive_sync);
            }
        }
        config.apply();
        self.pending = Some((config, serial));
        self.requested = Some(targets);
    }

    fn concluded(&mut self, event: &zwlr_output_configuration_v1::Event, qh: &QueueHandle<Self>) {
        let Some((config, serial)) = self.pending.take() else {
            return;
        };
        config.destroy();
        match event {
            // The kiosk describes the applied outputs with a new serial.
            zwlr_output_configuration_v1::Event::Succeeded => {
                self.accepted = self.requested.take();
                self.settled = true;
            }
            zwlr_output_configuration_v1::Event::Failed => {
                tracing::warn!("the kiosk rejected the greeter output configuration");
                self.rejected = Some(serial);
                self.settled = true;
            }
            // The outputs changed under the request; plan from the newer
            // description, which may already have arrived.
            _ => {
                if self.serial != Some(serial) {
                    self.reconsider(qh);
                }
            }
        }
    }
}

/// Configure the kiosk's outputs before winit creates the sign-in window,
/// then keep them configured on a watcher thread. A compositor without the
/// optional protocol keeps its own settings and the greeter uses the whole
/// window. Never fails: every problem is logged and leaves the kiosk's own
/// configuration in place.
#[must_use]
pub fn start() -> Displays {
    let Ok(connection) = Connection::connect_to_env() else {
        tracing::warn!("greeter outputs: no output connection");
        return Displays::default();
    };
    configure_connection(&connection).unwrap_or_else(|(error, displays)| {
        tracing::warn!(%error, "greeter outputs did not settle before the window opened");
        displays
    })
}

/// [`start`] on an established Wayland connection: configure until the
/// outputs settle (bounded), then keep them configured on a watcher thread.
pub fn configure_connection(connection: &Connection) -> Result<Displays, (&'static str, Displays)> {
    configure_connection_within(connection, STARTUP_LIMIT)
}

/// [`configure_connection`], waiting up to `limit` for the outputs to settle
/// rather than the sign-in window's own bound: a harness that answers the
/// request itself, on a loaded machine, waits as long as it needs.
///
/// # Errors
///
/// As [`configure_connection`].
pub fn configure_connection_within(
    connection: &Connection,
    limit: Duration,
) -> Result<Displays, (&'static str, Displays)> {
    let displays = Displays::default();
    let mut queue = connection.new_event_queue::<Outputs>();
    let mut state = Outputs::new(displays.clone());
    let settled = settle(connection, &mut queue, &mut state, limit);
    if state.manager.is_some() {
        watch(connection.clone(), queue, state);
    }
    match settled {
        Ok(()) => Ok(displays),
        Err(error) => Err((error, displays)),
    }
}

fn watch(connection: Connection, mut queue: EventQueue<Outputs>, mut state: Outputs) {
    let spawned = std::thread::Builder::new()
        .name("greeter-outputs".into())
        .spawn(move || {
            let _connection = connection;
            while queue.blocking_dispatch(&mut state).is_ok() {}
            tracing::warn!("greeter outputs: the output connection closed");
        });
    if spawned.is_err() {
        tracing::warn!("greeter outputs: hotplugged displays will not be configured");
    }
}

/// Run `queue` until the outputs are settled or `limit` passes.
fn settle(
    connection: &Connection,
    queue: &mut EventQueue<Outputs>,
    state: &mut Outputs,
    limit: Duration,
) -> Result<(), &'static str> {
    let deadline = Instant::now()
        .checked_add(limit)
        .ok_or("output deadline invalid")?;
    let qh = queue.handle();
    let _registry = connection.display().get_registry(&qh, ());
    let _sync = connection.display().sync(&qh, ());
    loop {
        queue
            .dispatch_pending(state)
            .map_err(|_| "output dispatch failed")?;
        if state.settled || (state.registry_done && state.manager.is_none()) {
            return Ok(());
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err("output configuration timed out");
        };
        let Some(reader) = queue.prepare_read() else {
            continue;
        };
        connection.flush().map_err(|_| "output flush failed")?;
        let backend = connection.backend();
        let descriptor = backend.poll_fd();
        let mut descriptors = [PollFd::new(&descriptor, PollFlags::IN)];
        let timeout = Timespec::try_from(remaining).map_err(|_| "output deadline invalid")?;
        let ready = poll(&mut descriptors, Some(&timeout)).map_err(|_| "output poll failed")?;
        if ready == 0 {
            return Err("output configuration timed out");
        }
        reader.read().map_err(|_| "output read failed")?;
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Outputs {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "zwlr_output_manager_v1"
            && state.manager.is_none()
        {
            state.manager = Some(registry.bind(name, version.min(4), qh, ()));
        }
    }
}
impl Dispatch<wl_callback::WlCallback, ()> for Outputs {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.registry_done = true;
    }
}
impl Dispatch<ZwlrOutputManagerV1, ()> for Outputs {
    fn event(
        state: &mut Self,
        _: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => state.heads.push(Head {
                object: head,
                facts: Facts::default(),
                current_mode: None,
                adaptive_sync: None,
            }),
            zwlr_output_manager_v1::Event::Done { serial } => {
                state.serial = Some(serial);
                state.reconsider(qh);
            }
            zwlr_output_manager_v1::Event::Finished => {
                state.manager = None;
                state.settled = true;
            }
            _ => (),
        }
    }
    wayland_client::event_created_child!(Outputs, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}
impl Dispatch<ZwlrOutputHeadV1, ()> for Outputs {
    fn event(
        state: &mut Self,
        output: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, zwlr_output_head_v1::Event::Finished) {
            // An unplugged display leaves the description; the manager's
            // following `done` re-plans the rest.
            state.heads.retain(|head| head.object != *output);
            if output.version() >= 3 {
                output.release();
            }
            return;
        }
        let Some(head) = state.heads.iter_mut().find(|head| head.object == *output) else {
            return;
        };
        let facts = &mut head.facts;
        match event {
            zwlr_output_head_v1::Event::Name { name } => facts.name = Some(name),
            zwlr_output_head_v1::Event::Enabled { enabled } => facts.enabled = Some(enabled != 0),
            zwlr_output_head_v1::Event::Position { x, y } => facts.position = Some((x, y)),
            zwlr_output_head_v1::Event::Transform {
                transform: WEnum::Value(transform),
            } => facts.transform = Some(transform),
            zwlr_output_head_v1::Event::Scale { scale } => facts.scale = Some(scale),
            zwlr_output_head_v1::Event::AdaptiveSync {
                state: WEnum::Value(adaptive_sync),
            } => {
                head.adaptive_sync = Some(adaptive_sync);
                facts.adaptive_sync_invalid = false;
            }
            zwlr_output_head_v1::Event::AdaptiveSync {
                state: WEnum::Unknown(_),
            } => {
                head.adaptive_sync = None;
                facts.adaptive_sync_invalid = true;
            }
            zwlr_output_head_v1::Event::PhysicalSize { width, height } => {
                facts.physical_size = u32::try_from(width).ok().zip(u32::try_from(height).ok());
            }
            zwlr_output_head_v1::Event::CurrentMode { mode } => head.current_mode = Some(mode),
            _ => (),
        }
    }
    wayland_client::event_created_child!(Outputs, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}
impl Dispatch<ZwlrOutputModeV1, ()> for Outputs {
    fn event(
        state: &mut Self,
        mode: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_mode_v1::Event::Size { width, height } => {
                let size = u32::try_from(width).ok().zip(u32::try_from(height).ok());
                if let Some(known) = state.modes.iter_mut().find(|known| known.mode == *mode) {
                    known.size = size;
                } else {
                    state.modes.push(Modes {
                        mode: mode.clone(),
                        size,
                    });
                }
            }
            zwlr_output_mode_v1::Event::Finished => {
                state.modes.retain(|known| known.mode != *mode);
                if mode.version() >= 3 {
                    mode.release();
                }
            }
            _ => (),
        }
    }
}
impl Dispatch<ZwlrOutputConfigurationV1, ()> for Outputs {
    fn event(
        state: &mut Self,
        _: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        state.concluded(&event, qh);
    }
}
wayland_client::delegate_noop!(Outputs: ignore ZwlrOutputConfigurationHeadV1);

#[cfg(test)]
mod tests {
    use super::*;

    fn head(name: &str, mode: (u32, u32), mm: Option<(u32, u32)>, x: i32, scale: f64) -> Facts {
        Facts {
            name: Some(name.into()),
            enabled: Some(true),
            position: Some((x, 0)),
            transform: Some(wl_output::Transform::Normal),
            scale: Some(scale),
            physical_size: mm,
            mode_size: Some(mode),
            adaptive_sync_invalid: false,
        }
    }

    /// The laptop this was reported on: a 2944x1840 310x200 mm panel and a
    /// 1080p monitor on a USB-C hub, as `cage` first places them at 1x.
    fn laptop_and_monitor() -> Vec<Facts> {
        vec![
            head("eDP-1", (2944, 1840), Some((310, 200)), 0, 1.0),
            head("DP-2", (1920, 1080), Some((530, 300)), 2944, 1.0),
        ]
    }

    #[test]
    fn each_head_gets_its_own_scale_and_the_layout_stays_contiguous() {
        let heads = laptop_and_monitor();
        let targets = plan(&heads).unwrap();
        let panel = targets[0].unwrap();
        let monitor = targets[1].unwrap();
        assert_eq!(
            panel.scale.to_bits(),
            quantized(settings_store::display_scale::derive_scale(
                (2944, 1840),
                Some((310, 200))
            ))
            .to_bits()
        );
        assert!(panel.scale > 1.0, "the high-DPI panel is scaled");
        assert_eq!(
            monitor.scale.to_bits(),
            quantized(settings_store::display_scale::derive_scale(
                (1920, 1080),
                Some((530, 300))
            ))
            .to_bits()
        );
        // The monitor starts where the scaled panel now ends: keeping the
        // kiosk's 1x position (2944) left a gap the centred card fell into.
        let (panel_width, _) = heads[0].logical_size(panel.scale).unwrap();
        assert_eq!(panel.position, (0, 0));
        assert_eq!(monitor.position, (panel_width, 0));
        assert!(differs(&heads, &targets));

        // Once applied, the same description needs nothing more.
        let mut applied = heads;
        for (head, target) in applied.iter_mut().zip(&targets) {
            let target = target.unwrap();
            head.position = Some(target.position);
            head.scale = Some(target.scale);
        }
        assert!(!differs(&applied, &plan(&applied).unwrap()));
    }

    #[test]
    fn a_hotplugged_display_joins_on_the_right_and_disabled_heads_stay_off() {
        let panel_scale = quantized(settings_store::display_scale::derive_scale(
            (2944, 1840),
            Some((310, 200)),
        ));
        let panel = head("eDP-1", (2944, 1840), Some((310, 200)), 0, panel_scale);
        let (panel_width, _) = panel.logical_size(panel_scale).unwrap();
        let mut off = head("DP-1", (1920, 1080), None, 0, 1.0);
        off.enabled = Some(false);
        off.position = None;
        // `cage` adds the new head to the right of the current layout.
        let plugged = head("DP-2", (3840, 2160), Some((600, 340)), panel_width, 1.0);
        let heads = vec![panel, off, plugged];
        let targets = plan(&heads).unwrap();
        assert_eq!(targets[1], None);
        let plugged = targets[2].unwrap();
        assert_eq!(plugged.position, (panel_width, 0));
        assert_eq!(
            plugged.scale.to_bits(),
            quantized(settings_store::display_scale::derive_scale(
                (3840, 2160),
                Some((600, 340))
            ))
            .to_bits()
        );
    }

    #[test]
    fn an_incomplete_description_configures_nothing() {
        let mut heads = laptop_and_monitor();
        heads[1].enabled = None;
        assert!(plan(&heads).is_err());
        let mut heads = laptop_and_monitor();
        heads[1].adaptive_sync_invalid = true;
        assert!(plan(&heads).is_err());
    }

    #[test]
    fn the_card_goes_on_the_built_in_panel_and_rotated_heads_swap_sides() {
        let mut heads = laptop_and_monitor();
        heads[1].position = Some((-1920, 0));
        heads[0].scale = Some(2.0);
        let arranged = arrangement(&heads);
        assert_eq!(
            arranged.extent(),
            Some(Rect::new(-1920.0, 0.0, 1472.0, 1080.0))
        );
        assert_eq!(
            arranged.in_window(),
            vec![
                Rect::new(0.0, 0.0, 1920.0, 1080.0),
                Rect::new(1920.0, 0.0, 3392.0, 920.0)
            ]
        );
        assert_eq!(
            arranged.primary_in_window(),
            Some(Rect::new(1920.0, 0.0, 3392.0, 920.0))
        );

        heads[0].name = Some("HDMI-A-1".into());
        assert_eq!(
            arrangement(&heads).primary_in_window(),
            Some(Rect::new(0.0, 0.0, 1920.0, 1080.0))
        );

        heads[1].transform = Some(wl_output::Transform::_90);
        assert_eq!(heads[1].logical_size(1.0), Some((1080, 1920)));
    }
}
