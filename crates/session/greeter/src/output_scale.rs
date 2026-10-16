//! Set the kiosk's output scale from genuine panel geometry before winit
//! creates the sign-in window. This speaks the compositor's standard
//! output-management protocol; it never changes mode, transform or power.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use wayland_client::backend::ObjectId;
use wayland_client::protocol::{wl_callback, wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

/// Startup is bounded even if an unavailable kiosk stops replying.
const STARTUP_LIMIT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Head {
    enabled: Option<bool>,
    position: Option<(i32, i32)>,
    transform: Option<wl_output::Transform>,
    scale: Option<f64>,
    adaptive_sync: Option<zwlr_output_head_v1::AdaptiveSyncState>,
    adaptive_sync_invalid: bool,
    physical_size: Option<(u32, u32)>,
    current_mode: Option<ZwlrOutputModeV1>,
    finished: bool,
}

#[derive(Default)]
struct Outputs {
    manager: Option<ZwlrOutputManagerV1>,
    heads: Vec<(ZwlrOutputHeadV1, Head)>,
    sizes: HashMap<ObjectId, (u32, u32)>,
    serial: Option<u32>,
    registry_done: bool,
    outcome: Option<bool>,
}

impl Outputs {
    fn desired_scale(&self, head: &Head) -> Option<f64> {
        let current = head.scale?;
        // An explicit kiosk scale is authoritative. Only repair the
        // unconfigured 1x case; unknown panel geometry keeps 1x.
        if current.to_bits() != 1.0_f64.to_bits() {
            return Some(current);
        }
        let mode = head.current_mode.as_ref()?;
        let pixels = *self.sizes.get(&mode.id())?;
        // Match the native desktop's panel policy on the raw mode geometry;
        // output transforms change placement, not the panel's measured DPI.
        Some(settings_store::display_scale::derive_scale(
            pixels,
            head.physical_size,
        ))
    }

    fn apply(&self, qh: &QueueHandle<Self>) -> Result<bool, &'static str> {
        let Some(manager) = &self.manager else {
            return Ok(false);
        };
        let serial = self.serial.ok_or("output description incomplete")?;
        let live: Vec<_> = self.heads.iter().filter(|(_, h)| !h.finished).collect();
        // Validate every head before creating a transaction. Partial
        // configurations can unintentionally turn another monitor off.
        let mut changed = false;
        for (_, head) in &live {
            match head.enabled {
                Some(false) => (),
                Some(true) => {
                    if head.position.is_none() || head.transform.is_none() {
                        return Err("output description incomplete");
                    }
                    if head.adaptive_sync_invalid {
                        return Err("output adaptive sync state unknown");
                    }
                    let scale = self
                        .desired_scale(head)
                        .ok_or("output description incomplete")?;
                    changed |= head.scale != Some(scale);
                }
                None => return Err("output description incomplete"),
            }
        }
        if !changed {
            return Ok(false);
        }
        let config = manager.create_configuration(serial, qh, ());
        for (output, head) in live {
            if head.enabled == Some(false) {
                config.disable_head(output);
                continue;
            }
            // Already validated above, and immutable throughout this request.
            let mode = head
                .current_mode
                .as_ref()
                .ok_or("output description incomplete")?;
            let (x, y) = head.position.ok_or("output description incomplete")?;
            let transform = head.transform.ok_or("output description incomplete")?;
            let scale = self
                .desired_scale(head)
                .ok_or("output description incomplete")?;
            let enabled = config.enable_head(output, qh, ());
            enabled.set_mode(mode);
            enabled.set_position(x, y);
            enabled.set_transform(transform);
            enabled.set_scale(scale);
            // A non-VRR head may omit this event even at version 4. Keep
            // its setting untouched; only copy an actually advertised value.
            if output.version() >= 4
                && let Some(adaptive_sync) = head.adaptive_sync
            {
                enabled.set_adaptive_sync(adaptive_sync);
            }
        }
        config.apply();
        Ok(true)
    }
}

/// Configure the actual greeter Wayland display. A compositor without this
/// optional protocol keeps its own settings.
///
/// # Errors
/// Returns a content-free platform error on rejection, timeout or transport failure.
pub fn configure() -> Result<(), &'static str> {
    let connection = Connection::connect_to_env().map_err(|_| "output connection unavailable")?;
    configure_connection(&connection)
}

/// The same production operation on an established Wayland connection.
///
/// # Errors
/// Returns a content-free platform error on rejection, timeout or transport failure.
pub fn configure_connection(connection: &Connection) -> Result<(), &'static str> {
    let deadline = Instant::now()
        .checked_add(STARTUP_LIMIT)
        .ok_or("output deadline invalid")?;
    let mut queue = connection.new_event_queue::<Outputs>();
    let qh = queue.handle();
    let _registry = connection.display().get_registry(&qh, ());
    let _sync = connection.display().sync(&qh, ());
    let mut state = Outputs::default();
    let mut applied = false;
    loop {
        queue
            .dispatch_pending(&mut state)
            .map_err(|_| "output dispatch failed")?;
        if state.registry_done && state.manager.is_none() {
            return Ok(());
        }
        if !applied && state.registry_done && state.serial.is_some() {
            if !state.apply(&qh)? {
                return Ok(());
            }
            applied = true;
        }
        if let Some(success) = state.outcome {
            return if success {
                Ok(())
            } else {
                Err("output scale rejected")
            };
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err("output scale timed out");
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
            return Err("output scale timed out");
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
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                state.heads.push((head, Head::default()));
            }
            zwlr_output_manager_v1::Event::Done { serial } => state.serial = Some(serial),
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
        let Some((_, head)) = state.heads.iter_mut().find(|(known, _)| known == output) else {
            return;
        };
        match event {
            zwlr_output_head_v1::Event::Enabled { enabled } => head.enabled = Some(enabled != 0),
            zwlr_output_head_v1::Event::Position { x, y } => head.position = Some((x, y)),
            zwlr_output_head_v1::Event::Transform {
                transform: WEnum::Value(transform),
            } => head.transform = Some(transform),
            zwlr_output_head_v1::Event::Scale { scale } => head.scale = Some(scale),
            zwlr_output_head_v1::Event::AdaptiveSync {
                state: WEnum::Value(adaptive_sync),
            } => head.adaptive_sync = Some(adaptive_sync),
            zwlr_output_head_v1::Event::AdaptiveSync {
                state: WEnum::Unknown(_),
            } => {
                head.adaptive_sync = None;
                head.adaptive_sync_invalid = true;
            }
            zwlr_output_head_v1::Event::PhysicalSize { width, height } => {
                head.physical_size = u32::try_from(width).ok().zip(u32::try_from(height).ok());
            }
            zwlr_output_head_v1::Event::CurrentMode { mode } => head.current_mode = Some(mode),
            zwlr_output_head_v1::Event::Finished => head.finished = true,
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
        if let zwlr_output_mode_v1::Event::Size { width, height } = event
            && let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height))
        {
            state.sizes.insert(mode.id(), (width, height));
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
        _: &QueueHandle<Self>,
    ) {
        state.outcome = match event {
            zwlr_output_configuration_v1::Event::Succeeded => Some(true),
            zwlr_output_configuration_v1::Event::Failed
            | zwlr_output_configuration_v1::Event::Cancelled => Some(false),
            _ => None,
        };
    }
}
wayland_client::delegate_noop!(Outputs: ignore ZwlrOutputConfigurationHeadV1);
