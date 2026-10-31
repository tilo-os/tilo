//! `wlr-output-management` against a headless compositor: a trusted display
//! tool sees the outputs, changes their scale and place, and is told when a
//! change cannot be made, turns one off and on again; a power tool switches a
//! panel off and on; ordinary clients do not see either global at all.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::Duration;

use tilo_compositor::{CONFIRM_FOR, Compositor, DisplayEvent, OutputConfig, StackPolicy};
use wayland_client::protocol::{
    wl_output, wl_output::WlOutput, wl_registry, wl_registry::WlRegistry,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, event_created_child};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, EVT_HEAD_OPCODE, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};
use wayland_protocols_wlr::output_power_management::v1::client::{
    zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1,
    zwlr_output_power_v1::{self, ZwlrOutputPowerV1},
};

#[derive(Default, Debug)]
struct Head {
    object: Option<ZwlrOutputHeadV1>,
    name: String,
    position: (i32, i32),
    scale: f64,
    modes: Vec<(i32, i32)>,
    enabled: Option<bool>,
}

#[derive(Default)]
struct Tool {
    globals: Vec<String>,
    manager: Option<ZwlrOutputManagerV1>,
    heads: Vec<Head>,
    serial: Option<u32>,
    outcome: Option<&'static str>,
    outputs: Vec<(WlOutput, String)>,
    power_manager: Option<ZwlrOutputPowerManagerV1>,
    power: Option<zwlr_output_power_v1::Mode>,
}

struct Client {
    connection: Connection,
    queue: EventQueue<Tool>,
    tool: Tool,
}

impl Client {
    fn connect(compositor: &mut Compositor, trusted: bool) -> Self {
        let socket = if trusted {
            compositor.connect_trusted_client()
        } else {
            compositor.connect_client()
        }
        .expect("client socket");
        let connection = Connection::from_socket(socket).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            tool: Tool::default(),
        }
    }

    fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.tool)
            .expect("client dispatch");
        self.connection.flush().expect("flush");
    }
}

fn pump(compositor: &mut Compositor, client: &mut Client, mut done: impl FnMut(&Tool) -> bool) {
    for _ in 0..500 {
        client.exchange();
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        client.exchange();
        if done(&client.tool) {
            return;
        }
    }
    panic!("condition not reached");
}

fn two_outputs() -> Compositor {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480).at(0, 0))
        .expect("output");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-2", 800, 600).at(640, 0))
        .expect("output");
    compositor
}

#[test]
fn ordinary_clients_cannot_configure_displays() {
    let mut compositor = two_outputs();
    let mut client = Client::connect(&mut compositor, false);
    pump(&mut compositor, &mut client, |tool| {
        tool.globals.iter().any(|global| global == "wl_compositor")
    });
    assert!(
        !client
            .tool
            .globals
            .iter()
            .any(|global| global == "zwlr_output_manager_v1")
    );
}

#[test]
fn a_display_tool_sees_the_outputs_and_rescales_and_moves_one() {
    let mut compositor = two_outputs();
    let mut client = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut client, |tool| {
        tool.serial.is_some() && tool.heads.len() == 2
    });
    let second = client
        .tool
        .heads
        .iter()
        .find(|head| head.name == "HEADLESS-2")
        .expect("a head per output");
    assert_eq!(second.position, (640, 0));
    assert_eq!(second.modes, vec![(800, 600)]);
    let head = second.object.clone().expect("head object");

    let handle = client.queue.handle();
    let manager = client.tool.manager.clone().expect("manager");
    let configuration =
        manager.create_configuration(client.tool.serial.expect("serial"), &handle, ());
    let changed = configuration.enable_head(&head, &handle, ());
    changed.set_scale(2.0);
    changed.set_position(0, 480);
    configuration.apply();
    pump(&mut compositor, &mut client, |tool| tool.outcome.is_some());
    assert_eq!(client.tool.outcome, Some("succeeded"));

    // The tool hears about the change like any other.
    pump(&mut compositor, &mut client, |tool| {
        tool.heads
            .iter()
            .any(|head| head.name == "HEADLESS-2" && head.position == (0, 480))
    });
    let second = client
        .tool
        .heads
        .iter()
        .find(|head| head.name == "HEADLESS-2")
        .expect("head");
    assert!((second.scale - 2.0).abs() < f64::EPSILON);
}

#[test]
fn turning_every_display_off_fails_and_changes_nothing() {
    let mut compositor = two_outputs();
    let mut client = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut client, |tool| {
        tool.serial.is_some() && tool.heads.len() == 2
    });
    let handle = client.queue.handle();
    let manager = client.tool.manager.clone().expect("manager");
    let configuration =
        manager.create_configuration(client.tool.serial.expect("serial"), &handle, ());
    for head in &client.tool.heads {
        configuration.disable_head(head.object.as_ref().expect("head object"));
    }
    configuration.apply();
    pump(&mut compositor, &mut client, |tool| tool.outcome.is_some());
    assert_eq!(client.tool.outcome, Some("failed"));
}

#[test]
fn a_display_tool_turns_an_output_off_and_on_again() {
    let mut compositor = two_outputs();
    let mut client = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut client, |tool| {
        tool.serial.is_some() && tool.heads.len() == 2
    });
    let head = |tool: &Tool| {
        tool.heads
            .iter()
            .find(|head| head.name == "HEADLESS-2")
            .and_then(|head| head.object.clone())
            .expect("head")
    };
    let handle = client.queue.handle();
    let manager = client.tool.manager.clone().expect("manager");
    let configuration =
        manager.create_configuration(client.tool.serial.expect("serial"), &handle, ());
    configuration.disable_head(&head(&client.tool));
    configuration.apply();
    pump(&mut compositor, &mut client, |tool| tool.outcome.is_some());
    assert_eq!(client.tool.outcome, Some("succeeded"));
    pump(&mut compositor, &mut client, |tool| {
        tool.heads
            .iter()
            .any(|head| head.name == "HEADLESS-2" && head.enabled == Some(false))
    });
    assert!(
        !compositor.output_lit("HEADLESS-2"),
        "a turned-off panel is dark"
    );
    assert!(compositor.output_lit("HEADLESS-1"));
    let shot = compositor.screenshot("HEADLESS-2").expect("screenshot");
    assert_eq!(shot.pixel(10, 10), Some([0, 0, 0, 255]));

    client.tool.outcome = None;
    let configuration =
        manager.create_configuration(client.tool.serial.expect("serial"), &handle, ());
    configuration.enable_head(&head(&client.tool), &handle, ());
    configuration.apply();
    pump(&mut compositor, &mut client, |tool| tool.outcome.is_some());
    assert_eq!(client.tool.outcome, Some("succeeded"));
    pump(&mut compositor, &mut client, |tool| {
        tool.heads
            .iter()
            .any(|head| head.name == "HEADLESS-2" && head.enabled == Some(true))
    });
    assert!(compositor.output_lit("HEADLESS-2"));
    compositor.render().expect("render");
    let shot = compositor.screenshot("HEADLESS-2").expect("screenshot");
    assert_ne!(
        shot.pixel(10, 10),
        Some([0, 0, 0, 255]),
        "lit again, it draws"
    );
}

#[test]
fn a_power_tool_switches_one_panel_off_and_on() {
    let mut compositor = two_outputs();
    let mut client = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut client, |tool| {
        tool.power_manager.is_some()
            && tool.outputs.len() == 2
            && tool.outputs.iter().all(|(_, name)| !name.is_empty())
    });
    let output = client
        .tool
        .outputs
        .iter()
        .find(|(_, name)| name == "HEADLESS-1")
        .map(|(output, _)| output.clone())
        .expect("output");
    let handle = client.queue.handle();
    let power = client
        .tool
        .power_manager
        .clone()
        .expect("power manager")
        .get_output_power(&output, &handle, ());
    pump(&mut compositor, &mut client, |tool| {
        tool.power == Some(zwlr_output_power_v1::Mode::On)
    });
    power.set_mode(zwlr_output_power_v1::Mode::Off);
    pump(&mut compositor, &mut client, |tool| {
        tool.power == Some(zwlr_output_power_v1::Mode::Off)
    });
    assert!(!compositor.output_lit("HEADLESS-1"));
    assert!(compositor.output_lit("HEADLESS-2"), "only that panel");
    power.set_mode(zwlr_output_power_v1::Mode::On);
    pump(&mut compositor, &mut client, |tool| {
        tool.power == Some(zwlr_output_power_v1::Mode::On)
    });
    assert!(compositor.output_lit("HEADLESS-1"));
}

#[test]
fn ordinary_clients_cannot_switch_panels_off() {
    let mut compositor = two_outputs();
    let mut client = Client::connect(&mut compositor, false);
    pump(&mut compositor, &mut client, |tool| {
        tool.globals.iter().any(|global| global == "wl_compositor")
    });
    assert!(
        !client
            .tool
            .globals
            .iter()
            .any(|global| global == "zwlr_output_power_manager_v1")
    );
}

#[test]
fn idle_blanking_darkens_every_panel_and_input_lights_them() {
    let mut compositor = two_outputs();
    compositor.render().expect("render");
    compositor.blank_now().expect("blank");
    assert!(!compositor.output_lit("HEADLESS-1"));
    assert!(!compositor.output_lit("HEADLESS-2"));
    let shot = compositor.screenshot("HEADLESS-1").expect("screenshot");
    assert_eq!(shot.pixel(10, 10), Some([0, 0, 0, 255]));
    compositor
        .input(tilo_compositor::InputEvent::PointerMotion { x: 5.0, y: 5.0 })
        .expect("input");
    assert!(compositor.output_lit("HEADLESS-1"));
    assert!(compositor.output_lit("HEADLESS-2"));
    assert!(
        compositor.take_session_events().is_empty(),
        "blanking is not locking"
    );
}

#[test]
fn idle_locking_is_handed_to_the_session() {
    let mut compositor = two_outputs();
    compositor.set_idle_config(tilo_compositor::IdleConfig {
        blank_timeout: None,
        lock_timeout: Some(Duration::ZERO),
        suspend_timeout: None,
    });
    compositor.dispatch(Duration::ZERO).expect("dispatch");
    assert_eq!(
        compositor.take_session_events(),
        vec![tilo_compositor::SessionTransition::Locked]
    );
    compositor.dispatch(Duration::ZERO).expect("dispatch");
    assert!(
        compositor.take_session_events().is_empty(),
        "once per episode"
    );
}

#[test]
fn a_configuration_made_against_an_old_serial_is_cancelled() {
    let mut compositor = two_outputs();
    let mut client = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut client, |tool| tool.serial.is_some());
    let handle = client.queue.handle();
    let manager = client.tool.manager.clone().expect("manager");
    let stale = client.tool.serial.expect("serial").wrapping_sub(1);
    let configuration = manager.create_configuration(stale, &handle, ());
    configuration.apply();
    pump(&mut compositor, &mut client, |tool| tool.outcome.is_some());
    assert_eq!(client.tool.outcome, Some("cancelled"));
}

impl Dispatch<WlRegistry, ()> for Tool {
    fn event(
        tool: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _connection: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "zwlr_output_manager_v1" => {
                    tool.manager = Some(registry.bind(name, 4, handle, ()));
                }
                "zwlr_output_power_manager_v1" => {
                    tool.power_manager = Some(registry.bind(name, 1, handle, ()));
                }
                "wl_output" => {
                    tool.outputs
                        .push((registry.bind(name, 4, handle, ()), String::new()));
                }
                _ => {}
            }
            tool.globals.push(interface);
        }
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for Tool {
    fn event(
        tool: &mut Self,
        _manager: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        (): &(),
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => tool.heads.push(Head {
                object: Some(head),
                ..Head::default()
            }),
            zwlr_output_manager_v1::Event::Done { serial } => tool.serial = Some(serial),
            _ => {}
        }
    }

    event_created_child!(Tool, ZwlrOutputManagerV1, [
        EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for Tool {
    fn event(
        tool: &mut Self,
        object: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        (): &(),
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        let Some(head) = tool
            .heads
            .iter_mut()
            .find(|head| head.object.as_ref() == Some(object))
        else {
            return;
        };
        match event {
            zwlr_output_head_v1::Event::Name { name } => head.name = name,
            zwlr_output_head_v1::Event::Position { x, y } => head.position = (x, y),
            zwlr_output_head_v1::Event::Scale { scale } => head.scale = scale,
            zwlr_output_head_v1::Event::Enabled { enabled } => head.enabled = Some(enabled != 0),
            _ => {}
        }
    }

    event_created_child!(Tool, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for Tool {
    fn event(
        tool: &mut Self,
        _mode: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        (): &(),
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        // Modes arrive right after their head's `mode` event, so they belong
        // to the last head that announced one.
        if let zwlr_output_mode_v1::Event::Size { width, height } = event
            && let Some(head) = tool
                .heads
                .iter_mut()
                .rev()
                .find(|head| !head.name.is_empty())
        {
            head.modes.push((width, height));
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ()> for Tool {
    fn event(
        tool: &mut Self,
        _configuration: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        (): &(),
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        tool.outcome = match event {
            zwlr_output_configuration_v1::Event::Succeeded => Some("succeeded"),
            zwlr_output_configuration_v1::Event::Failed => Some("failed"),
            zwlr_output_configuration_v1::Event::Cancelled => Some("cancelled"),
            _ => None,
        };
    }
}

impl Dispatch<WlOutput, ()> for Tool {
    fn event(
        tool: &mut Self,
        object: &WlOutput,
        event: wl_output::Event,
        (): &(),
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(output) = tool.outputs.iter_mut().find(|(known, _)| known == object)
        {
            output.1 = name;
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, ()> for Tool {
    fn event(
        tool: &mut Self,
        _power: &ZwlrOutputPowerV1,
        event: zwlr_output_power_v1::Event,
        (): &(),
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        if let zwlr_output_power_v1::Event::Mode {
            mode: wayland_client::WEnum::Value(mode),
        } = event
        {
            tool.power = Some(mode);
        }
    }
}

wayland_client::delegate_noop!(Tool: ignore ZwlrOutputConfigurationHeadV1);
wayland_client::delegate_noop!(Tool: ignore ZwlrOutputPowerManagerV1);

#[test]
fn the_sessions_settings_turn_an_output_off_rescale_another_and_set_the_layout() {
    let mut compositor = two_outputs();
    let mut settings = tilo_compositor::Settings::default();
    settings.display.disabled.insert("HEADLESS-2".into());
    settings.display.scales.insert("HEADLESS-1".into(), 2.0);
    settings.keyboard.layout = "de".into();
    compositor.apply_settings(&settings).expect("settings");
    assert!(!compositor.output_lit("HEADLESS-2"));
    let mut client = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut client, |tool| {
        tool.serial.is_some()
            && tool.heads.len() == 2
            && tool.heads.iter().all(|head| head.enabled.is_some())
    });
    let first = client
        .tool
        .heads
        .iter()
        .find(|head| head.name == "HEADLESS-1")
        .expect("head");
    assert!((first.scale - 2.0).abs() < f64::EPSILON);
    let second = client
        .tool
        .heads
        .iter()
        .find(|head| head.name == "HEADLESS-2")
        .expect("head");
    assert_eq!(second.enabled, Some(false));

    // Settings without the entry bring the output back.
    compositor
        .apply_settings(&tilo_compositor::Settings::default())
        .expect("settings");
    assert!(compositor.output_lit("HEADLESS-2"));
}

/// Rescale `name` to `scale` from the tool and wait for the answer.
fn rescale(
    compositor: &mut Compositor,
    client: &mut Client,
    name: &str,
    scale: f64,
) -> &'static str {
    let head = client
        .tool
        .heads
        .iter()
        .find(|head| head.name == name)
        .and_then(|head| head.object.clone())
        .expect("head");
    let handle = client.queue.handle();
    let manager = client.tool.manager.clone().expect("manager");
    client.tool.outcome = None;
    let configuration =
        manager.create_configuration(client.tool.serial.expect("serial"), &handle, ());
    configuration
        .enable_head(&head, &handle, ())
        .set_scale(scale);
    configuration.apply();
    pump(compositor, client, |tool| tool.outcome.is_some());
    client.tool.outcome.expect("outcome")
}

fn scale_of(client: &Client, name: &str) -> f64 {
    client
        .tool
        .heads
        .iter()
        .find(|head| head.name == name)
        .expect("head")
        .scale
}

#[test]
fn a_risky_change_asks_to_be_kept_and_a_reverted_one_is_not_put_back() {
    let mut compositor = two_outputs();
    let mut client = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut client, |tool| {
        tool.serial.is_some() && tool.heads.len() == 2
    });

    // Nobody looks at the second output: the change is simply kept.
    assert_eq!(
        rescale(&mut compositor, &mut client, "HEADLESS-2", 2.0),
        "succeeded"
    );
    let events = compositor.take_display_events();
    assert!(
        matches!(events.as_slice(), [DisplayEvent::Kept { changes }] if changes[0].scale == Some(2.0)),
        "{events:?}"
    );

    // The pointer is on the first: a scale it never showed is asked about.
    assert_eq!(
        rescale(&mut compositor, &mut client, "HEADLESS-1", 3.0),
        "succeeded"
    );
    let events = compositor.take_display_events();
    let [DisplayEvent::Confirm { id, within, .. }] = events.as_slice() else {
        panic!("{events:?}");
    };
    assert_eq!(*within, CONFIRM_FOR);
    pump(&mut compositor, &mut client, |tool| {
        (scale_of_tool(tool, "HEADLESS-1") - 3.0).abs() < f64::EPSILON
    });
    assert!(compositor.revert_display_change(*id).expect("revert"));
    pump(&mut compositor, &mut client, |tool| {
        (scale_of_tool(tool, "HEADLESS-1") - 1.0).abs() < f64::EPSILON
    });
    assert!(!compositor.keep_display_change(*id), "answered already");

    // Sending it again at once is refused, and the session hears once.
    assert_eq!(
        rescale(&mut compositor, &mut client, "HEADLESS-1", 3.0),
        "failed"
    );
    assert_eq!(
        rescale(&mut compositor, &mut client, "HEADLESS-1", 3.0),
        "failed"
    );
    let events = compositor.take_display_events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, DisplayEvent::RefusedReapply { .. }))
            .count(),
        1,
        "{events:?}"
    );
    assert!((scale_of(&client, "HEADLESS-1") - 1.0).abs() < f64::EPSILON);
}

#[test]
fn a_trusted_tool_changes_the_display_in_use_without_asking() {
    let mut compositor = two_outputs();
    compositor.set_trusted_display_tools([std::env::current_exe().expect("test binary")]);
    let mut client = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut client, |tool| {
        tool.serial.is_some() && tool.heads.len() == 2
    });
    assert_eq!(
        rescale(&mut compositor, &mut client, "HEADLESS-1", 3.0),
        "succeeded"
    );
    let events = compositor.take_display_events();
    assert!(
        matches!(events.as_slice(), [DisplayEvent::Kept { .. }]),
        "{events:?}"
    );
}

fn scale_of_tool(tool: &Tool, name: &str) -> f64 {
    tool.heads
        .iter()
        .find(|head| head.name == name)
        .map_or(0.0, |head| head.scale)
}
