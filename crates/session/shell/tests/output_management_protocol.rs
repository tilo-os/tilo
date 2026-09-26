//! `zwlr_output_manager_v1` and `zwlr_output_power_manager_v1` over the
//! wire, the way `wlr-randr`, `kanshi` and `wlopm` use them.
//!
//! The compositor lays its outputs out from a named headless fixture, a
//! client on the privileged socket reads the heads back, and a
//! configuration it applies must turn into the same staged
//! `DisplayTransaction` the Settings display pane writes — never a direct
//! modeset — with `succeeded` arriving only once the backend has applied
//! the plan.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use settings_store::hardware::{DisplayTransaction, DisplayTransactionState, OutputTransform};
use shell::compositor::backend::headless::output_fixture;
use shell::compositor::backend::{Output, OutputId};
use shell::compositor::output_globals::OutputGlobals;
use shell::compositor::output_heads::heads_from_outputs;
use shell::compositor::state::{ClientState, State};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use wayland_client::backend::ObjectId;
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};
use wayland_protocols_wlr::output_power_management::v1::client::{
    zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1,
    zwlr_output_power_v1::{self, ZwlrOutputPowerV1},
};

#[derive(Clone, Debug, Default)]
struct SeenHead {
    name: Option<String>,
    enabled: Option<bool>,
    position: Option<(i32, i32)>,
    transform: Option<wl_output::Transform>,
    scale: Option<f64>,
    modes: Vec<ZwlrOutputModeV1>,
    current_mode: Option<ZwlrOutputModeV1>,
    finished: bool,
}

#[derive(Default)]
struct Tool {
    globals: Vec<String>,
    manager: Option<ZwlrOutputManagerV1>,
    power_manager: Option<ZwlrOutputPowerManagerV1>,
    outputs: Vec<wl_output::WlOutput>,
    heads: Vec<(ZwlrOutputHeadV1, SeenHead)>,
    mode_sizes: HashMap<ObjectId, (i32, i32)>,
    serial: Option<u32>,
    outcome: Option<&'static str>,
    power: Vec<Result<zwlr_output_power_v1::Mode, ()>>,
}

impl Tool {
    fn head_mut(&mut self, head: &ZwlrOutputHeadV1) -> &mut SeenHead {
        &mut self
            .heads
            .iter_mut()
            .find(|(bound, _)| bound == head)
            .expect("event for an announced head")
            .1
    }

    fn head(&self, name: &str) -> &(ZwlrOutputHeadV1, SeenHead) {
        self.heads
            .iter()
            .find(|(_, seen)| seen.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no head named {name}"))
    }

    fn live_heads(&self) -> Vec<&SeenHead> {
        self.heads
            .iter()
            .map(|(_, seen)| seen)
            .filter(|seen| !seen.finished)
            .collect()
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Tool {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "zwlr_output_manager_v1" => {
                state.manager = Some(registry.bind(name, version.min(4), qh, ()));
            }
            "zwlr_output_power_manager_v1" => {
                state.power_manager = Some(registry.bind(name, 1, qh, ()));
            }
            "wl_output" => state
                .outputs
                .push(registry.bind(name, version.min(4), qh, ())),
            _ => {}
        }
        state.globals.push(interface);
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for Tool {
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
                state.heads.push((head, SeenHead::default()));
            }
            zwlr_output_manager_v1::Event::Done { serial } => state.serial = Some(serial),
            _ => {}
        }
    }

    wayland_client::event_created_child!(Tool, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for Tool {
    fn event(
        state: &mut Self,
        head: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seen = state.head_mut(head);
        match event {
            zwlr_output_head_v1::Event::Name { name } => seen.name = Some(name),
            zwlr_output_head_v1::Event::Enabled { enabled } => seen.enabled = Some(enabled != 0),
            zwlr_output_head_v1::Event::Position { x, y } => seen.position = Some((x, y)),
            zwlr_output_head_v1::Event::Transform {
                transform: WEnum::Value(transform),
            } => seen.transform = Some(transform),
            zwlr_output_head_v1::Event::Scale { scale } => seen.scale = Some(scale),
            zwlr_output_head_v1::Event::Mode { mode } => seen.modes.push(mode),
            zwlr_output_head_v1::Event::CurrentMode { mode } => seen.current_mode = Some(mode),
            zwlr_output_head_v1::Event::Finished => seen.finished = true,
            _ => {}
        }
    }

    wayland_client::event_created_child!(Tool, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for Tool {
    fn event(
        state: &mut Self,
        mode: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_output_mode_v1::Event::Size { width, height } = event {
            state.mode_sizes.insert(mode.id(), (width, height));
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ()> for Tool {
    fn event(
        state: &mut Self,
        _: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.outcome = Some(match event {
            zwlr_output_configuration_v1::Event::Succeeded => "succeeded",
            zwlr_output_configuration_v1::Event::Failed => "failed",
            zwlr_output_configuration_v1::Event::Cancelled => "cancelled",
            _ => "unknown",
        });
    }
}

impl Dispatch<ZwlrOutputPowerV1, ()> for Tool {
    fn event(
        state: &mut Self,
        _: &ZwlrOutputPowerV1,
        event: zwlr_output_power_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_power_v1::Event::Mode {
                mode: WEnum::Value(mode),
            } => state.power.push(Ok(mode)),
            zwlr_output_power_v1::Event::Failed => state.power.push(Err(())),
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(Tool: ignore wl_output::WlOutput);
wayland_client::delegate_noop!(Tool: ignore ZwlrOutputPowerManagerV1);
wayland_client::delegate_noop!(Tool: ignore ZwlrOutputConfigurationHeadV1);

/// A compositor laid out from a fixture, with one client connected.
struct Harness {
    _event_loop: EventLoop<'static, State>,
    display: Display<State>,
    state: State,
    _globals: OutputGlobals,
    outputs: Vec<Output>,
    conn: Connection,
    queue: EventQueue<Tool>,
    tool: Tool,
}

impl Harness {
    fn new(fixture: &str, client: Arc<ClientState>) -> Self {
        let outputs = output_fixture(fixture).expect("fixture");
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        state.set_output_layout(&outputs);
        let mut globals = OutputGlobals::new();
        globals.sync::<State>(&display.handle(), &outputs, None);
        state.set_output_heads(heads_from_outputs(&outputs));

        let (client_side, server_side) = UnixStream::pair().expect("socketpair");
        display
            .handle()
            .insert_client(server_side, client)
            .expect("insert client");
        let conn = Connection::from_socket(client_side).expect("client connection");
        let queue = conn.new_event_queue::<Tool>();
        let _registry = conn.display().get_registry(&queue.handle(), ());
        let mut harness = Self {
            _event_loop: event_loop,
            display,
            state,
            _globals: globals,
            outputs,
            conn,
            queue,
            tool: Tool::default(),
        };
        let expected = harness.outputs.len();
        harness.pump("registry", |tool| tool.outputs.len() == expected);
        harness
    }

    /// A privileged tool that has read every head.
    fn privileged(fixture: &str) -> Self {
        let mut harness = Self::new(fixture, ClientState::privileged_arc());
        let expected = harness.outputs.len();
        harness.pump("heads", |tool| {
            tool.serial.is_some()
                && tool.heads.len() == expected
                && tool
                    .heads
                    .iter()
                    .all(|(_, seen)| seen.scale.is_some() && seen.current_mode.is_some())
        });
        harness
    }

    fn pump(&mut self, label: &str, done: impl Fn(&Tool) -> bool) {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(3))
            .expect("deadline");
        while Instant::now() < deadline {
            let _ = self.conn.flush();
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.tool);
            if done(&self.tool) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("output-management walk stalled at {label}");
    }

    fn configuration(&mut self) -> ZwlrOutputConfigurationV1 {
        let serial = self.tool.serial.expect("serial");
        self.configuration_at(serial)
    }

    fn configuration_at(&mut self, serial: u32) -> ZwlrOutputConfigurationV1 {
        self.tool.outcome = None;
        let manager = self.tool.manager.as_ref().expect("manager bound");
        manager.create_configuration(serial, &self.queue.handle(), ())
    }

    fn enable(
        &self,
        configuration: &ZwlrOutputConfigurationV1,
        name: &str,
    ) -> ZwlrOutputConfigurationHeadV1 {
        configuration.enable_head(&self.tool.head(name).0, &self.queue.handle(), ())
    }

    fn outcome(&mut self, label: &str) -> &'static str {
        self.pump(label, |tool| tool.outcome.is_some());
        self.tool.outcome.expect("outcome")
    }
}

#[test]
fn heads_mirror_the_compositor_outputs() {
    let harness = Harness::privileged("docked-laptop");
    let heads = harness.tool.live_heads();
    assert_eq!(heads.len(), 2);
    for (seen, output) in heads.iter().zip(&harness.outputs) {
        assert_eq!(seen.enabled, Some(true));
        assert_eq!(seen.position, Some(output.logical_position));
        assert_eq!(seen.transform, Some(wl_output::Transform::Normal));
        assert!((seen.scale.expect("scale") - f64::from(output.scale)).abs() < 1e-3);
        let current = seen.current_mode.as_ref().expect("current mode");
        let (w, h) = output.physical_size();
        assert_eq!(
            harness.tool.mode_sizes.get(&current.id()),
            Some(&(i32::try_from(w).unwrap(), i32::try_from(h).unwrap()))
        );
    }
    assert_eq!(heads[0].name.as_deref(), Some("VIRTUAL-1"));
    assert_eq!(heads[1].name.as_deref(), Some("VIRTUAL-2"));
}

#[test]
fn a_sandboxed_client_sees_neither_display_configuration_global() {
    let harness = Harness::new("docked-laptop", Arc::new(ClientState::default()));
    for interface in ["zwlr_output_manager_v1", "zwlr_output_power_manager_v1"] {
        assert!(
            !harness.tool.globals.iter().any(|g| g == interface),
            "{interface} reached a client on the public socket: {:?}",
            harness.tool.globals
        );
    }
}

#[test]
fn apply_stages_the_settings_transaction_and_succeeds_once_the_backend_applied_it() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged("docked-laptop");
    let configuration = harness.configuration();
    let _panel = harness.enable(&configuration, "VIRTUAL-1");
    let monitor = harness.enable(&configuration, "VIRTUAL-2");
    monitor.set_position(0, -1440);
    monitor.set_scale(1.25);
    monitor.set_transform(wl_output::Transform::_90);
    configuration.apply();
    harness.pump("apply reaches the compositor", |_| {
        DisplayTransaction::load().is_some()
    });

    let mut transaction = DisplayTransaction::load().expect("a staged transaction");
    assert_eq!(transaction.state, DisplayTransactionState::Requested);
    let monitor = transaction
        .candidate
        .output_for("VIRTUAL-2")
        .expect("monitor");
    assert!(monitor.enabled);
    assert_eq!((monitor.x, monitor.y), (0, -1440));
    assert!((monitor.scale - 1.25).abs() < 1e-3);
    assert_eq!(monitor.transform, OutputTransform::Rotate90);
    let panel = transaction
        .candidate
        .output_for("VIRTUAL-1")
        .expect("panel");
    assert!(panel.enabled);
    assert!((panel.scale - 2.0).abs() < 1e-3);
    assert_eq!(harness.tool.outcome, None, "nothing is applied yet");
    assert!(harness.state.output_management.take_staged_by_tool());

    // The DRM backend validates and applies the plan, awaiting the user's
    // confirmation — the moment the tool learns it worked.
    transaction.state = DisplayTransactionState::AwaitingConfirmation;
    transaction.save().expect("save");
    harness
        .state
        .output_management
        .resolve_transactions(DisplayTransaction::load().as_ref());
    assert_eq!(harness.outcome("apply outcome"), "succeeded");
}

#[test]
fn a_plan_the_backend_rejects_fails_the_apply() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged("docked-laptop");
    let configuration = harness.configuration();
    let _panel = harness.enable(&configuration, "VIRTUAL-1");
    configuration.disable_head(&harness.tool.head("VIRTUAL-2").0);
    configuration.apply();
    harness.pump("apply reaches the compositor", |_| {
        DisplayTransaction::load().is_some()
    });
    let mut transaction = DisplayTransaction::load().expect("staged");
    assert_eq!(
        transaction
            .candidate
            .output_for("VIRTUAL-2")
            .map(|o| o.enabled),
        Some(false)
    );
    transaction.state = DisplayTransactionState::Rejected;
    transaction.save().expect("save");
    harness
        .state
        .output_management
        .resolve_transactions(DisplayTransaction::load().as_ref());
    assert_eq!(harness.outcome("apply outcome"), "failed");
}

#[test]
fn test_validates_without_staging_anything() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged("docked-laptop");

    let configuration = harness.configuration();
    let panel = harness.enable(&configuration, "VIRTUAL-1");
    panel.set_scale(1.5);
    let _monitor = harness.enable(&configuration, "VIRTUAL-2");
    configuration.test();
    assert_eq!(harness.outcome("valid test"), "succeeded");

    // Every screen off is what Settings refuses too.
    let configuration = harness.configuration();
    for name in ["VIRTUAL-1", "VIRTUAL-2"] {
        configuration.disable_head(&harness.tool.head(name).0);
    }
    configuration.test();
    assert_eq!(harness.outcome("all-off test"), "failed");

    // A head left out of the configuration is not "unchanged".
    let configuration = harness.configuration();
    let _panel = harness.enable(&configuration, "VIRTUAL-1");
    configuration.test();
    assert_eq!(harness.outcome("partial test"), "failed");

    assert!(
        DisplayTransaction::load().is_none(),
        "test must never stage a display change"
    );
}

#[test]
fn a_configuration_made_against_old_heads_is_cancelled() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged("docked-laptop");
    let stale = harness.tool.serial.expect("serial");

    // Unplug the monitor: the tool is told the head is gone and why its
    // configuration no longer applies.
    let remaining: Vec<Output> = harness.outputs.iter().take(1).cloned().collect();
    harness
        .state
        .set_output_heads(heads_from_outputs(&remaining));
    harness.pump("head removal", |tool| {
        tool.serial.is_some_and(|serial| serial != stale) && tool.live_heads().len() == 1
    });

    let configuration = harness.configuration_at(stale);
    let _panel = harness.enable(&configuration, "VIRTUAL-1");
    configuration.apply();
    assert_eq!(harness.outcome("stale apply"), "cancelled");
    assert!(DisplayTransaction::load().is_none());
}

#[test]
fn output_power_switches_one_output_and_reports_it() {
    let mut harness = Harness::privileged("docked-laptop");
    let power = harness
        .tool
        .power_manager
        .as_ref()
        .expect("power manager")
        .get_output_power(&harness.tool.outputs[1], &harness.queue.handle(), ());
    harness.pump("initial power mode", |tool| !tool.power.is_empty());
    assert_eq!(harness.tool.power, vec![Ok(zwlr_output_power_v1::Mode::On)]);

    power.set_mode(zwlr_output_power_v1::Mode::Off);
    harness.pump("power off", |tool| tool.power.len() == 2);
    assert_eq!(harness.tool.power[1], Ok(zwlr_output_power_v1::Mode::Off));
    assert_eq!(
        harness.state.output_management.take_power_requests(),
        vec![(OutputId(1), false)]
    );

    // The output going away fails the object rather than leaving it
    // controlling nothing.
    let remaining: Vec<Output> = harness.outputs.iter().take(1).cloned().collect();
    harness
        .state
        .set_output_heads(heads_from_outputs(&remaining));
    harness.pump("power object failed", |tool| tool.power.len() == 3);
    assert_eq!(harness.tool.power[2], Err(()));
}
