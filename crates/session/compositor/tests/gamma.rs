//! Gamma ramps against a headless compositor, whose outputs apply them as a
//! display engine would: night light tints what the screen shows; a trusted
//! gamma tool (`zwlr_gamma_control_v1`) holds an output's ramp exclusively,
//! replacing night light until it lets go; ordinary clients do not see the
//! global.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::time::Duration;

use tilo_compositor::{Compositor, OutputConfig, StackPolicy};
use wayland_client::protocol::{wl_output::WlOutput, wl_registry, wl_registry::WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_wlr::gamma_control::v1::client::{
    zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1,
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};

/// The compositor's background, as drawn.
const BACKGROUND: [u8; 4] = [26, 27, 38, 255];

#[derive(Default)]
struct Tool {
    globals: Vec<String>,
    output: Option<WlOutput>,
    manager: Option<ZwlrGammaControlManagerV1>,
    /// Per control created: its size once told, whether it failed.
    controls: Vec<(Option<u32>, bool)>,
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

    fn control(&mut self) -> ZwlrGammaControlV1 {
        let index = self.tool.controls.len();
        self.tool.controls.push((None, false));
        self.tool
            .manager
            .as_ref()
            .expect("manager")
            .get_gamma_control(
                self.tool.output.as_ref().expect("output"),
                &self.queue.handle(),
                index,
            )
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

fn shown(compositor: &mut Compositor) -> [u8; 4] {
    compositor.render().expect("render");
    let frame = compositor.screenshot("HEADLESS-1").expect("screenshot");
    frame.pixel(10, 10).expect("pixel")
}

fn compositor() -> Compositor {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 64, 48))
        .expect("output");
    compositor
}

/// A table of three ramps of `size`: red full, green half, blue zero.
fn table(size: u32) -> std::fs::File {
    let size = usize::try_from(size).unwrap();
    let ramp = |scale: f64| -> Vec<u8> {
        (0..size)
            .flat_map(|i| {
                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a test ramp of a few hundred entries"
                )]
                let value = (i as f64 / size.saturating_sub(1) as f64 * scale * f64::from(u16::MAX))
                    .round() as u16;
                value.to_ne_bytes()
            })
            .collect()
    };
    let mut file = tempfile::tempfile().expect("table file");
    for bytes in [ramp(1.0), ramp(0.5), ramp(0.0)] {
        file.write_all(&bytes).expect("write table");
    }
    file
}

#[test]
fn ordinary_clients_cannot_set_gamma() {
    let mut compositor = compositor();
    let mut client = Client::connect(&mut compositor, false);
    pump(&mut compositor, &mut client, |tool| {
        tool.globals.iter().any(|global| global == "wl_compositor")
    });
    assert!(
        !client
            .tool
            .globals
            .iter()
            .any(|global| global == "zwlr_gamma_control_manager_v1")
    );
}

#[test]
fn night_light_warms_the_screen_and_turns_off_again() {
    let mut compositor = compositor();
    assert_eq!(shown(&mut compositor), BACKGROUND);
    compositor
        .set_night_light(Some(2_000))
        .expect("night light");
    let warm = shown(&mut compositor);
    assert_eq!(warm[0], BACKGROUND[0], "red stays");
    assert!(
        warm[1] < BACKGROUND[1] && warm[2] < BACKGROUND[2],
        "{warm:?}"
    );
    assert!(
        BACKGROUND[2] - warm[2] > BACKGROUND[1] - warm[1],
        "blue falls most"
    );
    compositor.set_night_light(None).expect("night light off");
    assert_eq!(shown(&mut compositor), BACKGROUND);
}

#[test]
fn a_gamma_tool_holds_the_ramp_exclusively_until_it_lets_go() {
    let mut compositor = compositor();
    compositor
        .set_night_light(Some(2_000))
        .expect("night light");
    let warm = shown(&mut compositor);
    let mut client = Client::connect(&mut compositor, true);
    pump(&mut compositor, &mut client, |tool| {
        tool.manager.is_some() && tool.output.is_some()
    });

    let control = client.control();
    pump(&mut compositor, &mut client, |tool| {
        tool.controls[0].0.is_some()
    });
    let size = client.tool.controls[0].0.unwrap();
    assert_eq!(size, 256);
    let file = table(size);
    control.set_gamma(file.as_fd());
    pump(&mut compositor, &mut client, |_| true);
    compositor
        .dispatch(Duration::from_millis(2))
        .expect("dispatch");
    let tool = shown(&mut compositor);
    assert_eq!(tool[0], BACKGROUND[0]);
    assert!(tool[1].abs_diff(BACKGROUND[1] / 2) <= 1, "{tool:?}");
    assert_eq!(tool[2], 0, "the tool's ramp replaces night light");

    // A second control on the same output fails at once.
    let second = client.control();
    pump(&mut compositor, &mut client, |tool| tool.controls[1].1);
    second.destroy();

    // Letting go puts night light back.
    control.destroy();
    pump(&mut compositor, &mut client, |_| true);
    compositor
        .dispatch(Duration::from_millis(2))
        .expect("dispatch");
    assert_eq!(shown(&mut compositor), warm);
}

impl Dispatch<WlRegistry, ()> for Tool {
    fn event(
        tool: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "wl_output" => tool.output = Some(registry.bind(name, 1, queue, ())),
                "zwlr_gamma_control_manager_v1" => {
                    tool.manager = Some(registry.bind(name, 1, queue, ()));
                }
                _ => {}
            }
            tool.globals.push(interface);
        }
    }
}

impl Dispatch<WlOutput, ()> for Tool {
    fn event(
        _: &mut Self,
        _: &WlOutput,
        _: wayland_client::protocol::wl_output::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for Tool {
    fn event(
        _: &mut Self,
        _: &ZwlrGammaControlManagerV1,
        _: <ZwlrGammaControlManagerV1 as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrGammaControlV1, usize> for Tool {
    fn event(
        tool: &mut Self,
        _: &ZwlrGammaControlV1,
        event: zwlr_gamma_control_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let control = &mut tool.controls[*index];
        match event {
            zwlr_gamma_control_v1::Event::GammaSize { size } => control.0 = Some(size),
            zwlr_gamma_control_v1::Event::Failed => control.1 = true,
            _ => {}
        }
    }
}
