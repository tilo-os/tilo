//! `zwlr_output_manager_v1` and `zwlr_output_power_manager_v1` over the
//! wire, the way `wlr-randr`, `kanshi` and `wlopm` use them.
//!
//! The compositor lays its outputs out from a named headless fixture, a
//! client on the privileged socket reads the heads back, and a
//! configuration it applies must turn into the same staged
//! `DisplayTransaction` the Settings display pane writes — never a direct
//! modeset — with `succeeded` arriving only once the backend has applied
//! the plan.
//!
//! An applied change is kept straight away, the way every other compositor
//! treats these tools. Only a risky one — a custom mode, a display the
//! person is using turned off or given a mode it never showed — waits for
//! them: the shell's Keep/Revert prompt appears on every display, unless
//! the tool is one they trusted. A tool that puts a reverted layout
//! straight back is refused, with one notice, instead of asking again.

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

use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
use settings_store::hardware::{DisplayTransaction, DisplayTransactionState, OutputTransform};
use shell::compositor::backend::headless::output_fixture;
use shell::compositor::backend::{Output, OutputId};
use shell::compositor::output_globals::OutputGlobals;
use shell::compositor::output_heads::heads_from_outputs;
use shell::compositor::output_management::{DisplayConfirmation, DisplayToolNotice};
use shell::compositor::state::{ClientState, State};
use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::overlay_host::{OutputGeometry, ShellOverlayHost};
use shell::shell::state::WorkspaceTree;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, Role};
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
use winit::keyboard::{Key, ModifiersState, NamedKey};

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
    output_names: HashMap<ObjectId, String>,
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
            .find(|(_, seen)| seen.name.as_deref() == Some(name) && !seen.finished)
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

impl Dispatch<wl_output::WlOutput, ()> for Tool {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.output_names.insert(output.id(), name);
        }
    }
}

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
        Self::privileged_tool(fixture, None)
    }

    /// A privileged tool whose connection revealed `executable`.
    fn privileged_tool(fixture: &str, executable: Option<&str>) -> Self {
        let mut harness = Self::new(
            fixture,
            ClientState::privileged_tool_arc(executable.map(str::to_owned)),
        );
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
        let deadline = tilo_test_env::hang_deadline();
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
fn apply_stages_the_settings_transaction_and_keeps_a_safe_change_once_applied() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));
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
    assert_eq!(
        harness.state.output_management.take_confirmation(),
        None,
        "nothing to keep before the backend applied it"
    );

    // The DRM backend validates and applies the plan — the moment the tool
    // learns it worked. Moving, scaling and rotating the monitor nobody is
    // using is kept without asking.
    transaction.state = DisplayTransactionState::AwaitingConfirmation;
    transaction.save().expect("save");
    harness
        .state
        .output_management
        .resolve_transactions(DisplayTransaction::load().as_ref());
    assert_eq!(harness.outcome("apply outcome"), "succeeded");
    assert_eq!(harness.state.output_management.take_confirmation(), None);
    assert_eq!(state_now(), DisplayTransactionState::ConfirmRequested);
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

const KANSHI: &str = "/usr/bin/kanshi";

fn unix_now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis(),
    )
    .expect("milliseconds fit u64")
}

/// The monitor's size, and a refresh rate 0.2 Hz off the 144 Hz it lists.
const UNLISTED_MODE: (i32, i32, i32) = (2560, 1440, 143_800);

/// The tool asks for the monitor at a refresh rate the connector does not
/// list — close enough to resolve to its 144 Hz mode, but risky; the backend
/// applies the plan and starts the restore countdown, ending at
/// `deadline_unix_ms`.
fn apply_and_await(harness: &mut Harness, deadline_unix_ms: u64) -> DisplayTransaction {
    apply_custom_mode(harness);
    await_confirmation(harness, deadline_unix_ms, |_| {})
}

fn apply_custom_mode(harness: &mut Harness) {
    let configuration = harness.configuration();
    let _panel = harness.enable(&configuration, "VIRTUAL-1");
    let monitor = harness.enable(&configuration, "VIRTUAL-2");
    let (width, height, refresh) = UNLISTED_MODE;
    monitor.set_custom_mode(width, height, refresh);
    configuration.apply();
    harness.pump("apply reaches the compositor", |_| {
        DisplayTransaction::load()
            .is_some_and(|transaction| transaction.state == DisplayTransactionState::Requested)
    });
}

/// Play the backend for the staged transaction: `edit` what it applied,
/// then wait for the person until `deadline_unix_ms`.
fn await_confirmation(
    harness: &mut Harness,
    deadline_unix_ms: u64,
    edit: impl FnOnce(&mut DisplayTransaction),
) -> DisplayTransaction {
    let mut transaction = DisplayTransaction::load().expect("a staged transaction");
    edit(&mut transaction);
    transaction.state = DisplayTransactionState::AwaitingConfirmation;
    transaction.deadline_unix_ms = Some(deadline_unix_ms);
    transaction.save().expect("save");
    harness
        .state
        .output_management
        .resolve_transactions(DisplayTransaction::load().as_ref());
    assert_eq!(harness.outcome("apply outcome"), "succeeded");
    transaction
}

/// A countdown no test outlives: tests end it themselves when they need it
/// ended, so how long the host takes to get there cannot end it early.
fn countdown_outlasting_the_test() -> u64 {
    let bound = u64::try_from(tilo_test_env::hang_bound().as_millis()).expect("bound fits u64");
    unix_now_ms().saturating_add(bound.saturating_mul(2))
}

/// The shell, laid out over the fixture's displays, showing the prompt the
/// compositor handed it.
fn prompt_host(
    harness: &Harness,
    confirmation: DisplayConfirmation,
) -> (ShellOverlayHost, (TaffyTree<()>, NodeId)) {
    let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&WorkspaceTree::bootstrap_demo());
    let geometry: Vec<OutputGeometry> = harness
        .outputs
        .iter()
        .map(|output| {
            OutputGeometry::unscaled(
                f64::from(output.logical_position.0),
                f64::from(output.logical_position.1),
                f64::from(output.size.0),
                f64::from(output.size.1),
            )
        })
        .collect();
    host.set_output_geometry(&geometry);
    let layout = crate::display_layout_switcher::layout_host(&mut host);
    host.show_display_confirmation(confirmation.transaction, confirmation.tool);
    (host, layout)
}

fn key(host: &mut ShellOverlayHost, layout: &(TaffyTree<()>, NodeId), key: NamedKey) {
    crate::display_layout_switcher::press(host, layout, Key::Named(key), ModifiersState::empty());
}

fn prompt_dialog(host: &ShellOverlayHost) -> AccessNode {
    use ui::widget::Widget;
    host.accessibility()
        .expect("the host publishes a semantic tree")
        .children
        .into_iter()
        .find(|node| {
            node.role == Role::Dialog
                && node
                    .name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("Keep the display change"))
        })
        .expect("the prompt is in the semantic tree")
}

fn state_now() -> DisplayTransactionState {
    DisplayTransaction::load().expect("transaction").state
}

#[test]
fn a_kanshi_like_tool_lights_up_a_hotplugged_monitor_without_asking() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));

    // Undock, then dock again: the monitor comes back as a new head.
    let panel_only: Vec<Output> = harness.outputs.iter().take(1).cloned().collect();
    harness
        .state
        .set_output_heads(heads_from_outputs(&panel_only));
    harness.pump("undock", |tool| tool.live_heads().len() == 1);
    let undocked = harness.tool.serial;
    let docked = harness.outputs.clone();
    harness.state.set_output_heads(heads_from_outputs(&docked));
    harness.pump("dock", |tool| {
        tool.serial != undocked
            && tool.live_heads().len() == 2
            && tool.live_heads().iter().all(|h| !h.modes.is_empty())
    });

    // kanshi's profile matches: the monitor at its listed mode, left of
    // the panel, at 1.25.
    let configuration = harness.configuration();
    let _panel = harness.enable(&configuration, "VIRTUAL-1");
    let monitor = harness.enable(&configuration, "VIRTUAL-2");
    let listed = harness
        .tool
        .head("VIRTUAL-2")
        .1
        .modes
        .first()
        .expect("a listed mode")
        .clone();
    monitor.set_mode(&listed);
    monitor.set_position(-2048, 0);
    monitor.set_scale(1.25);
    configuration.apply();
    harness.pump("apply reaches the compositor", |_| {
        DisplayTransaction::load().is_some()
    });
    await_confirmation(&mut harness, countdown_outlasting_the_test(), |_| {});
    assert_eq!(
        harness.state.output_management.take_confirmation(),
        None,
        "no card for a listed mode on a freshly plugged monitor"
    );
    assert_eq!(state_now(), DisplayTransactionState::ConfirmRequested);
}

#[test]
fn a_refresh_rate_the_connector_does_not_list_shows_the_card() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));
    let transaction = apply_and_await(&mut harness, countdown_outlasting_the_test());
    assert_eq!(
        harness.state.output_management.take_confirmation(),
        Some(DisplayConfirmation {
            transaction: transaction.id,
            tool: Some(DisplayTool::new(KANSHI)),
        }),
        "the person is asked to keep it, naming the tool"
    );
    assert_eq!(state_now(), DisplayTransactionState::AwaitingConfirmation);
}

#[test]
fn turning_off_the_display_under_the_pointer_shows_the_card() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));
    // The pointer starts on the panel.
    let configuration = harness.configuration();
    configuration.disable_head(&harness.tool.head("VIRTUAL-1").0);
    let _monitor = harness.enable(&configuration, "VIRTUAL-2");
    configuration.apply();
    harness.pump("apply reaches the compositor", |_| {
        DisplayTransaction::load().is_some()
    });
    await_confirmation(&mut harness, countdown_outlasting_the_test(), |_| {});
    assert!(
        harness
            .state
            .output_management
            .take_confirmation()
            .is_some()
    );
    assert_eq!(state_now(), DisplayTransactionState::AwaitingConfirmation);
}

#[test]
fn a_tool_reapplying_a_reverted_layout_is_refused_with_one_notice() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));
    apply_and_await(&mut harness, countdown_outlasting_the_test());
    let confirmation = harness.state.output_management.take_confirmation();
    let (mut host, layout) = prompt_host(&harness, confirmation.expect("asked"));
    key(&mut host, &layout, NamedKey::Escape);
    assert_eq!(state_now(), DisplayTransactionState::RevertRequested);
    let reverted = DisplayTransaction::load().expect("transaction").id;

    // kanshi sees the layout come back and applies its profile again.
    for attempt in 0..2 {
        let configuration = harness.configuration();
        let _panel = harness.enable(&configuration, "VIRTUAL-1");
        let monitor = harness.enable(&configuration, "VIRTUAL-2");
        let (width, height, refresh) = UNLISTED_MODE;
        monitor.set_custom_mode(width, height, refresh);
        configuration.apply();
        assert_eq!(harness.outcome("reapply"), "failed", "attempt {attempt}");
        assert_eq!(
            DisplayTransaction::load().expect("transaction").id,
            reverted,
            "nothing new is staged"
        );
        assert_eq!(harness.state.output_management.take_confirmation(), None);
        let notices = harness.state.output_management.take_notices();
        if attempt == 0 {
            assert_eq!(
                notices,
                [DisplayToolNotice::ReappliedAfterRevert {
                    tool: Some(DisplayTool::new(KANSHI)),
                }]
            );
        } else {
            assert!(notices.is_empty(), "the person is told once");
        }
    }

    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&WorkspaceTree::bootstrap_demo());
    host.report_refused_display_reapply(Some(&DisplayTool::new(KANSHI)));
    assert!(
        chrome
            .borrow()
            .notifications()
            .iter()
            .any(|n| n.title == "kanshi tried to reapply a display layout you reverted")
    );

    // A different layout from the same tool is not what was reverted.
    let configuration = harness.configuration();
    let _panel = harness.enable(&configuration, "VIRTUAL-1");
    let monitor = harness.enable(&configuration, "VIRTUAL-2");
    monitor.set_position(0, -1440);
    configuration.apply();
    harness.pump("a new layout is staged", |_| {
        DisplayTransaction::load().is_some_and(|transaction| transaction.id != reverted)
    });
}

#[test]
fn an_external_apply_asks_on_every_display_and_enter_keeps_it() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));
    apply_and_await(&mut harness, countdown_outlasting_the_test());
    let confirmation = harness
        .state
        .output_management
        .take_confirmation()
        .expect("the person is asked");
    let (mut host, layout) = prompt_host(&harness, confirmation);

    assert!(host.display_confirmation_visible());
    let cards = host.display_confirmation_cards();
    assert_eq!(cards.len(), harness.outputs.len(), "one card per display");
    for (card, output) in cards.iter().zip(&harness.outputs) {
        let (x, y) = output.logical_position;
        let centre = card.card.center();
        assert!(
            centre.x >= f64::from(x)
                && centre.x <= f64::from(x) + f64::from(output.size.0)
                && centre.y >= f64::from(y)
                && centre.y <= f64::from(y) + f64::from(output.size.1),
            "each card sits on its own display"
        );
    }
    let dialog = prompt_dialog(&host);
    assert_eq!(
        dialog.name.as_deref(),
        Some("Keep the display change from kanshi?")
    );
    assert!(dialog.state.modal);
    let buttons: Vec<&str> = dialog
        .children
        .iter()
        .filter_map(|node| node.name.as_deref())
        .collect();
    assert_eq!(
        buttons,
        ["Keep", "Revert", "Keep, and don't ask again for kanshi"]
    );

    key(&mut host, &layout, NamedKey::Enter);
    assert_eq!(state_now(), DisplayTransactionState::ConfirmRequested);
    assert!(!host.display_confirmation_visible());
    assert!(
        !TrustedDisplayTools::load().contains(&DisplayTool::new(KANSHI)),
        "Keep answers for this change only"
    );
}

#[test]
fn escape_restores_the_previous_layout() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));
    apply_and_await(&mut harness, countdown_outlasting_the_test());
    let confirmation = harness.state.output_management.take_confirmation();
    let (mut host, layout) = prompt_host(&harness, confirmation.expect("asked"));
    key(&mut host, &layout, NamedKey::Escape);
    assert_eq!(state_now(), DisplayTransactionState::RevertRequested);
    assert!(!host.display_confirmation_visible());
}

#[test]
fn running_out_of_time_restores_the_previous_layout() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));
    apply_and_await(&mut harness, countdown_outlasting_the_test());
    let confirmation = harness.state.output_management.take_confirmation();
    let (mut host, _layout) = prompt_host(&harness, confirmation.expect("asked"));
    assert!(!host.poll_display_confirmation(), "time is left");

    let mut transaction = DisplayTransaction::load().expect("transaction");
    transaction.deadline_unix_ms = Some(unix_now_ms().saturating_sub(1));
    transaction.save().expect("save");
    assert!(host.poll_display_confirmation(), "the countdown ended");
    assert_eq!(state_now(), DisplayTransactionState::RevertRequested);
    assert!(!host.display_confirmation_visible());
}

#[test]
fn an_answer_given_elsewhere_closes_the_prompt() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));
    apply_and_await(&mut harness, countdown_outlasting_the_test());
    let confirmation = harness.state.output_management.take_confirmation();
    let (mut host, _layout) = prompt_host(&harness, confirmation.expect("asked"));

    // Settings › Displays kept it first.
    let mut transaction = DisplayTransaction::load().expect("transaction");
    transaction.request_confirmation().expect("keep");
    assert!(host.poll_display_confirmation());
    assert!(!host.display_confirmation_visible());
    assert_eq!(state_now(), DisplayTransactionState::ConfirmRequested);
}

#[test]
fn always_keep_trusts_the_tool_and_its_next_change_is_kept_without_asking() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));
    apply_and_await(&mut harness, countdown_outlasting_the_test());
    let confirmation = harness.state.output_management.take_confirmation();
    let (mut host, layout) = prompt_host(&harness, confirmation.expect("asked"));
    key(&mut host, &layout, NamedKey::Tab);
    key(&mut host, &layout, NamedKey::Tab);
    assert!(
        prompt_dialog(&host)
            .children
            .iter()
            .any(|node| node.state.focused
                && node.name.as_deref() == Some("Keep, and don't ask again for kanshi")),
        "Tab walks Keep, Revert, Always keep"
    );
    key(&mut host, &layout, NamedKey::Enter);
    assert_eq!(state_now(), DisplayTransactionState::ConfirmRequested);
    assert!(TrustedDisplayTools::load().contains(&DisplayTool::new(KANSHI)));

    let second = apply_and_await(&mut harness, countdown_outlasting_the_test());
    assert_eq!(
        harness.state.output_management.take_confirmation(),
        None,
        "a trusted tool is not asked about"
    );
    let kept = DisplayTransaction::load().expect("transaction");
    assert_eq!(kept.id, second.id);
    assert_eq!(kept.state, DisplayTransactionState::ConfirmRequested);
}

#[test]
fn another_tool_is_still_asked_about() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    TrustedDisplayTools::trust(DisplayTool::new(KANSHI)).expect("trust");
    let mut harness = Harness::privileged_tool("docked-laptop", Some("/usr/bin/wlr-randr"));
    apply_and_await(&mut harness, countdown_outlasting_the_test());
    assert_eq!(
        harness
            .state
            .output_management
            .take_confirmation()
            .and_then(|confirmation| confirmation.tool),
        Some(DisplayTool::new("/usr/bin/wlr-randr"))
    );
    assert_eq!(state_now(), DisplayTransactionState::AwaitingConfirmation);
}

#[test]
fn a_change_that_leaves_no_display_on_is_never_kept_on_trust() {
    let _home = tilo_test_env::TestHome::new().expect("test home");
    TrustedDisplayTools::trust(DisplayTool::new(KANSHI)).expect("trust");
    let mut harness = Harness::privileged_tool("docked-laptop", Some(KANSHI));

    // Asking for every screen off is refused outright.
    let configuration = harness.configuration();
    for name in ["VIRTUAL-1", "VIRTUAL-2"] {
        configuration.disable_head(&harness.tool.head(name).0);
    }
    configuration.apply();
    assert_eq!(harness.outcome("all-off apply"), "failed");
    assert!(DisplayTransaction::load().is_none());

    // And an applied plan that ends with every head off still waits for
    // the person, however trusted the tool.
    let configuration = harness.configuration();
    let _panel = harness.enable(&configuration, "VIRTUAL-1");
    let _monitor = harness.enable(&configuration, "VIRTUAL-2");
    configuration.apply();
    harness.pump("apply reaches the compositor", |_| {
        DisplayTransaction::load().is_some()
    });
    await_confirmation(
        &mut harness,
        countdown_outlasting_the_test(),
        |transaction| {
            for output in transaction.candidate.outputs.values_mut() {
                output.enabled = false;
            }
        },
    );
    assert!(
        harness
            .state
            .output_management
            .take_confirmation()
            .is_some(),
        "the person is asked"
    );
    assert_eq!(state_now(), DisplayTransactionState::AwaitingConfirmation);
}

#[test]
fn wl_output_names_are_the_head_names() {
    let mut harness = Harness::privileged("docked-laptop");
    let expected = harness.outputs.len();
    harness.pump("output names", |tool| tool.output_names.len() == expected);
    let mut outputs: Vec<&str> = harness
        .tool
        .outputs
        .iter()
        .map(|output| harness.tool.output_names[&output.id()].as_str())
        .collect();
    let mut heads: Vec<&str> = harness
        .tool
        .live_heads()
        .iter()
        .filter_map(|head| head.name.as_deref())
        .collect();
    outputs.sort_unstable();
    heads.sort_unstable();
    assert_eq!(outputs, heads);
    assert_eq!(outputs, ["VIRTUAL-1", "VIRTUAL-2"]);
}
