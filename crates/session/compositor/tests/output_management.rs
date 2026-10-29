//! `wlr-output-management` against a headless compositor: a trusted display
//! tool sees the outputs, changes their scale and place, and is told when a
//! change cannot be made; ordinary clients do not see the global at all.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::Duration;

use tilo_compositor::{Compositor, OutputConfig, StackPolicy};
use wayland_client::protocol::{wl_registry, wl_registry::WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, event_created_child};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, EVT_HEAD_OPCODE, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

#[derive(Default, Debug)]
struct Head {
    object: Option<ZwlrOutputHeadV1>,
    name: String,
    position: (i32, i32),
    scale: f64,
    modes: Vec<(i32, i32)>,
}

#[derive(Default)]
struct Tool {
    globals: Vec<String>,
    manager: Option<ZwlrOutputManagerV1>,
    heads: Vec<Head>,
    serial: Option<u32>,
    outcome: Option<&'static str>,
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
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
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
            if interface == "zwlr_output_manager_v1" {
                tool.manager = Some(registry.bind(name, 4, handle, ()));
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
            && let Some(head) = tool.heads.iter_mut().rev().find(|head| !head.name.is_empty())
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

wayland_client::delegate_noop!(Tool: ignore ZwlrOutputConfigurationHeadV1);
