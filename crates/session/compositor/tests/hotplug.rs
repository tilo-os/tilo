//! An output that appears and disappears while the compositor runs, as a
//! connector plugged into a running session does: it is lit and placed
//! beside the outputs already there, clients are told it is there, the desk
//! is told the monitors it now has, and pulling it out takes it away again
//! and closes the hole it left.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use tilo_compositor::{
    Compositor, LayoutPoint, LayoutRect, OutputChange, OutputConfig, WindowInfo, WindowPolicy,
};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};

/// The scene's background, as a frame read back shows it: what a lit output
/// draws where nothing else is.
const BACKGROUND: [u8; 4] = [26, 27, 38, 255];

/// Each output and the monitor on it, as the policy is told them.
type Told = Vec<(LayoutRect, String)>;

/// An output's name and the rectangle it covers in the layout, when it has
/// a place there.
type Placed = (String, Option<(i32, i32, i32, i32)>);

/// A policy that keeps the monitors it is last told of: the desk is told
/// them whenever the outputs change.
struct Monitors(Rc<RefCell<Told>>);

impl WindowPolicy for Monitors {
    fn initial_size(
        &mut self,
        _window: &WindowInfo,
        _outputs: &[LayoutRect],
    ) -> Option<(i32, i32)> {
        None
    }

    fn place(
        &mut self,
        _window: &WindowInfo,
        _size: (i32, i32),
        _outputs: &[LayoutRect],
    ) -> LayoutPoint {
        LayoutPoint::from((0, 0))
    }

    fn monitors(&mut self, monitors: &[(LayoutRect, String)]) {
        *self.0.borrow_mut() = monitors.to_vec();
    }
}

/// A client that only reads the registry: which globals exist right now.
#[derive(Default)]
struct Registry {
    globals: Vec<(u32, String)>,
}

impl Dispatch<WlRegistry, ()> for Registry {
    fn event(
        state: &mut Self,
        _registry: &WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name, interface, ..
            } => state.globals.push((name, interface)),
            wl_registry::Event::GlobalRemove { name } => {
                state.globals.retain(|(global, _)| *global != name);
            }
            _ => {}
        }
    }
}

struct Client {
    connection: Connection,
    queue: EventQueue<Registry>,
    seen: Registry,
}

impl Client {
    fn connect(compositor: &mut Compositor) -> Self {
        let socket = compositor.connect_client().expect("client socket");
        let connection = Connection::from_socket(socket).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            seen: Registry::default(),
        }
    }

    fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.seen)
            .expect("client dispatch");
        self.connection.flush().expect("flush");
    }

    /// How many outputs the compositor shows clients right now.
    fn outputs(&self) -> usize {
        self.seen
            .globals
            .iter()
            .filter(|(_, interface)| interface == "wl_output")
            .count()
    }
}

fn pump(compositor: &mut Compositor, client: &mut Client, mut done: impl FnMut(&Registry) -> bool) {
    for _ in 0..500 {
        client.exchange();
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        client.exchange();
        if done(&client.seen) {
            return;
        }
    }
    panic!("condition not reached");
}

/// A compositor with one output, as a session runs with its panel alone.
fn one_output(told: &Rc<RefCell<Told>>) -> Compositor {
    let mut compositor =
        Compositor::headless(Box::new(Monitors(Rc::clone(told)))).expect("headless");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor.render().expect("render");
    compositor
}

fn rects(compositor: &Compositor) -> Vec<Placed> {
    compositor
        .outputs()
        .into_iter()
        .map(|output| (output.name, output.rect))
        .collect()
}

/// What the policy was last told, as rectangles and monitors.
fn told_monitors(told: &Rc<RefCell<Told>>) -> Vec<((i32, i32, i32, i32), String)> {
    told.borrow()
        .iter()
        .map(|(rect, monitor)| {
            (
                (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h),
                monitor.clone(),
            )
        })
        .collect()
}

#[test]
fn an_output_plugged_in_while_the_desk_runs_is_lit_beside_the_others_and_told_to_the_desk() {
    let told = Rc::new(RefCell::new(Vec::new()));
    let mut compositor = one_output(&told);
    let mut client = Client::connect(&mut compositor);
    pump(&mut compositor, &mut client, |seen| {
        !seen.globals.is_empty()
    });
    assert_eq!(client.outputs(), 1);
    assert_eq!(
        rects(&compositor),
        [("HEADLESS-1".into(), Some((0, 0, 640, 480)))]
    );

    // A connector appears mid-run, asking for no place of its own: a second
    // screen shows it beside the one already there, never over it, where
    // the desk would take it for a mirror of that one and give it nothing.
    compositor
        .add_output(&OutputConfig::new("HEADLESS-2", 1280, 720))
        .expect("output");

    assert!(compositor.output_lit("HEADLESS-2"), "a new output is lit");
    assert_eq!(
        rects(&compositor),
        [
            ("HEADLESS-1".into(), Some((0, 0, 640, 480))),
            ("HEADLESS-2".into(), Some((640, 0, 1280, 720))),
        ],
        "the new output is placed beside the other, which stays where it is"
    );
    let monitors = told_monitors(&told);
    assert_eq!(
        monitors,
        [
            ((0, 0, 640, 480), "HEADLESS-1".to_owned()),
            ((640, 0, 1280, 720), "HEADLESS-2".to_owned()),
        ],
        "the desk is told the monitors it now has"
    );
    pump(&mut compositor, &mut client, |seen| {
        seen.globals
            .iter()
            .filter(|(_, interface)| interface == "wl_output")
            .count()
            == 2
    });
    assert_eq!(client.outputs(), 2, "clients see the new output");

    // It draws, rather than sitting dark until something else does.
    compositor.render().expect("render");
    let shot = compositor.screenshot("HEADLESS-2").expect("screenshot");
    assert_eq!(shot.pixel(1279, 719), Some(BACKGROUND));
}

#[test]
fn an_output_unplugged_while_the_desk_runs_leaves_the_lit_set_and_the_layout() {
    let told = Rc::new(RefCell::new(Vec::new()));
    let mut compositor = one_output(&told);
    compositor
        .add_output(&OutputConfig::new("HEADLESS-2", 1280, 720))
        .expect("output");
    let mut client = Client::connect(&mut compositor);
    pump(&mut compositor, &mut client, |seen| {
        seen.globals
            .iter()
            .filter(|(_, interface)| interface == "wl_output")
            .count()
            == 2
    });

    // The cable is pulled: the output is gone, not merely dark.
    compositor.remove_output("HEADLESS-2").expect("unplug");

    assert!(
        !compositor.output_lit("HEADLESS-2"),
        "an output that is gone is not lit"
    );
    assert!(compositor.output_lit("HEADLESS-1"));
    assert_eq!(
        rects(&compositor),
        [("HEADLESS-1".into(), Some((0, 0, 640, 480)))],
        "what clients and Settings are shown no longer lists it"
    );
    let monitors = told_monitors(&told);
    assert_eq!(
        monitors,
        [((0, 0, 640, 480), "HEADLESS-1".to_owned())],
        "the desk is told the monitor set it has now"
    );
    assert!(
        compositor.screenshot("HEADLESS-2").is_err(),
        "nothing draws there any more"
    );
    pump(&mut compositor, &mut client, |seen| {
        seen.globals
            .iter()
            .filter(|(_, interface)| interface == "wl_output")
            .count()
            == 1
    });
    assert_eq!(client.outputs(), 1, "clients are told it is gone");

    // Pulling the same cable twice fails rather than taking the other one.
    assert!(compositor.remove_output("HEADLESS-2").is_err());
    assert_eq!(rects(&compositor).len(), 1);
}

#[test]
fn the_place_an_unplugged_output_left_closes_over() {
    let told = Rc::new(RefCell::new(Vec::new()));
    let mut compositor = one_output(&told);
    compositor
        .add_output(&OutputConfig::new("HEADLESS-2", 1280, 720))
        .expect("output");
    assert_eq!(
        rects(&compositor)[1].1,
        Some((640, 0, 1280, 720)),
        "the second screen is to the right"
    );

    // The one on the left goes: the one left sits where the pointer and the
    // desk look, not 640 pixels into an empty layout.
    compositor.remove_output("HEADLESS-1").expect("unplug");
    assert_eq!(
        rects(&compositor),
        [("HEADLESS-2".into(), Some((0, 0, 1280, 720)))]
    );
    let monitors = told_monitors(&told);
    assert_eq!(
        monitors,
        [((0, 0, 1280, 720), "HEADLESS-2".to_owned())],
        "the desk follows it to where it now is"
    );
    assert!(compositor.output_lit("HEADLESS-2"));

    // Plugged back in, it takes its place beside the one that stayed.
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    let placed: Vec<Placed> = rects(&compositor);
    assert_eq!(
        placed,
        [
            ("HEADLESS-2".into(), Some((0, 0, 1280, 720))),
            ("HEADLESS-1".into(), Some((1280, 0, 640, 480))),
        ],
        "an output plugged back in is beside the others, whichever came first"
    );
}

#[test]
fn an_output_the_person_turned_off_keeps_no_place_when_another_is_plugged_in() {
    let told = Rc::new(RefCell::new(Vec::new()));
    let mut compositor = one_output(&told);
    compositor
        .add_output(&OutputConfig::new("HEADLESS-2", 1280, 720))
        .expect("output");
    compositor
        .configure_outputs(&[OutputChange::disable("HEADLESS-1")])
        .expect("turn off");
    assert!(!compositor.output_lit("HEADLESS-1"));

    // Another connector appears while that display is off: it takes the
    // place the layout has, and the one turned off stays off.
    compositor
        .add_output(&OutputConfig::new("HEADLESS-3", 800, 600))
        .expect("output");
    assert!(
        !compositor.output_lit("HEADLESS-1"),
        "a display turned off is not lit by another being plugged in"
    );
    assert!(compositor.output_lit("HEADLESS-3"));
    assert_eq!(
        rects(&compositor),
        [
            ("HEADLESS-2".into(), Some((0, 0, 1280, 720))),
            ("HEADLESS-3".into(), Some((1280, 0, 800, 600))),
            ("HEADLESS-1".into(), None),
        ],
        "the turned-off display is still known, and takes no place"
    );
}
