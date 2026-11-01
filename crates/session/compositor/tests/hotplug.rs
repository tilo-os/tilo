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

/// Each output and the monitor on it, and the primary monitor, as the
/// policy is told them.
type Told = (Vec<(LayoutRect, String)>, Option<String>);

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

    fn monitors(&mut self, monitors: &[(LayoutRect, String)], primary: Option<&str>) {
        *self.0.borrow_mut() = (monitors.to_vec(), primary.map(str::to_owned));
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
        .0
        .iter()
        .map(|(rect, monitor)| {
            (
                (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h),
                monitor.clone(),
            )
        })
        .collect()
}

/// The primary monitor the policy was last told, if any.
fn told_primary(told: &Rc<RefCell<Told>>) -> Option<String> {
    told.borrow().1.clone()
}

#[test]
fn an_output_plugged_in_while_the_desk_runs_is_lit_beside_the_others_and_told_to_the_desk() {
    let told = Rc::new(RefCell::new(Told::default()));
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
    let told = Rc::new(RefCell::new(Told::default()));
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
    let told = Rc::new(RefCell::new(Told::default()));
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
    let told = Rc::new(RefCell::new(Told::default()));
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

/// A configuration that makes `primary` the display the person chose, and
/// turns `off` off.
fn choosing(primary: &str, off: &[&str]) -> tilo_compositor::DisplayConfig {
    tilo_compositor::DisplayConfig {
        primary: Some(primary.to_owned()),
        disabled: off.iter().map(|name| (*name).to_owned()).collect(),
        ..tilo_compositor::DisplayConfig::default()
    }
}

/// The display the person made primary is the one the desk is told is
/// primary, and the choice waits for the display to come back rather than
/// falling to whichever output is lit.
#[test]
fn the_primary_chosen_in_settings_is_told_to_the_desk_and_returns_with_its_display() {
    let told = Rc::new(RefCell::new(Told::default()));
    let mut compositor = one_output(&told);
    compositor
        .add_output(&OutputConfig::new("HEADLESS-2", 1280, 720))
        .expect("output");
    assert_eq!(told_primary(&told), None, "no choice made yet");

    compositor
        .set_display_config(&choosing("HEADLESS-2", &[]))
        .expect("a primary");
    assert_eq!(
        told_primary(&told).as_deref(),
        Some("HEADLESS-2"),
        "the desk is told the display the person chose"
    );

    // The cable is pulled: the other display stands in, and the choice is
    // kept for the one that went away.
    compositor.remove_output("HEADLESS-2").expect("unplug");
    assert_eq!(
        told_primary(&told),
        None,
        "a display that is gone is not the primary"
    );
    compositor
        .add_output(&OutputConfig::new("HEADLESS-2", 1280, 720))
        .expect("plugged back in");
    assert_eq!(
        told_primary(&told).as_deref(),
        Some("HEADLESS-2"),
        "the display is primary again the moment it comes back"
    );
}

/// A display turned off is never the primary one: the desk is told of none
/// while the chosen display is dark.
#[test]
fn a_display_turned_off_is_never_the_primary_one() {
    let told = Rc::new(RefCell::new(Told::default()));
    let mut compositor = one_output(&told);
    compositor
        .add_output(&OutputConfig::new("HEADLESS-2", 1280, 720))
        .expect("output");
    compositor
        .set_display_config(&choosing("HEADLESS-2", &[]))
        .expect("a primary");
    assert_eq!(told_primary(&told).as_deref(), Some("HEADLESS-2"));

    compositor
        .set_display_config(&choosing("HEADLESS-2", &["HEADLESS-2"]))
        .expect("the display turned off");
    assert!(!compositor.output_lit("HEADLESS-2"), "the display is off");
    assert_eq!(
        told_primary(&told),
        None,
        "a display turned off is not the primary, even when it was chosen"
    );

    // Turned back on, it is the primary again.
    compositor
        .set_display_config(&choosing("HEADLESS-2", &[]))
        .expect("a primary");
    assert!(compositor.output_lit("HEADLESS-2"));
    assert_eq!(told_primary(&told).as_deref(), Some("HEADLESS-2"));
}
