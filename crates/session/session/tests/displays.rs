//! The native session across two displays, with real Wayland clients: each
//! display holds its own windows, new windows open on the display with
//! focus, focus and windows move between displays through the desk's
//! published actions, a mirror is one display, and dialogs float over the
//! window they belong to instead of taking a tab.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

mod common;

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::time::Duration;

use tilo_actions::ResourceSpec;
use tilo_compositor::{Compositor, InputEvent, OutputConfig};
use tilo_desktop::desk_verbs::{CHROME_RESOURCE, DESK_RESOURCE, DISPLAY_KIND, WINDOW_KIND};
use tilo_desktop_model::WindowWidth;
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_registry::WlRegistry, wl_shm,
    wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;

/// The colour the compositor paints the test windows' pixels, read back as
/// RGBA, and the same colour as the bytes of an `ARGB8888` buffer hold them.
const WINDOW: [u8; 3] = [40, 40, 200];
const WINDOW_BYTES: [u8; 4] = [200, 40, 40, 255];

/// What one toplevel was told.
#[derive(Default)]
struct Toplevel {
    configured: bool,
    size: Option<(i32, i32)>,
}

#[derive(Default)]
struct Seen {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    toplevels: Vec<Toplevel>,
}

/// One client with toplevels of a solid colour, the later ones possibly
/// dialogs of the earlier.
struct TestClient {
    connection: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    windows: Vec<(WlSurface, XdgToplevel)>,
}

impl TestClient {
    fn connect(compositor: &mut Compositor) -> Self {
        let socket = compositor.connect_client().expect("client socket");
        let connection = Connection::from_socket(socket).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            seen: Seen::default(),
            windows: Vec::new(),
        }
    }

    /// Send what is queued, read what arrived, without blocking.
    fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            // Nothing to read is fine: WouldBlock.
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.seen)
            .expect("dispatch");
        self.connection.flush().expect("flush");
    }

    /// Open toplevel number `self.windows.len()` of `app_id`, set up by
    /// `before` before its first commit. Returns its index.
    fn open(&mut self, app_id: &str, before: impl FnOnce(&XdgToplevel)) -> usize {
        let index = self.windows.len();
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let xdg = self
            .seen
            .wm_base
            .as_ref()
            .expect("xdg_wm_base")
            .get_xdg_surface(&surface, &qh, index);
        let toplevel = xdg.get_toplevel(&qh, index);
        toplevel.set_app_id(app_id.into());
        before(&toplevel);
        surface.commit();
        self.seen.toplevels.push(Toplevel::default());
        self.windows.push((surface, toplevel));
        index
    }

    /// Attach a buffer of one colour to toplevel `index` and commit it.
    fn show(&self, index: usize, width: i32, height: i32) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).unwrap();
        let fd =
            rustix::fs::memfd_create("tilo-session-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        let mut file = std::fs::File::from(fd);
        file.write_all(&WINDOW_BYTES.repeat(len / 4)).unwrap();
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * height,
            &qh,
            (),
        );
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        let surface = &self.windows[index].0;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
        pool.destroy();
    }
}

/// Let the session and `client` talk until `done` holds.
#[track_caller]
fn pump(
    session: &mut Session,
    client: &mut TestClient,
    mut done: impl FnMut(&mut Session, &TestClient) -> bool,
) {
    common::wait_for(session, "what the test asked of the session", |session| {
        client.exchange();
        let answer = done(session, client);
        client.exchange();
        answer.then_some(())
    });
}

/// A session on `outputs`, each `WIDTH` x `HEIGHT` at its place.
fn session(places: &[(i32, i32)]) -> Session {
    let outputs: Vec<OutputConfig> = places
        .iter()
        .enumerate()
        .map(|(index, place)| {
            OutputConfig::new(format!("HEADLESS-{}", index + 1), WIDTH, HEIGHT).at(place.0, place.1)
        })
        .collect();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &outputs,
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// Two displays side by side.
fn docked() -> Session {
    session(&[(0, 0), (WIDTH, 0)])
}

fn client(session: &mut Session) -> TestClient {
    let mut client = TestClient::connect(session.compositor());
    pump(session, &mut client, |_, client| {
        client.seen.wm_base.is_some() && client.seen.shm.is_some()
    });
    client
}

/// Open toplevel of `app_id` (set up by `before`), drawn at the size it
/// was configured to, else `fallback`, and wait until it is mapped.
fn map(
    session: &mut Session,
    client: &mut TestClient,
    app_id: &str,
    fallback: (i32, i32),
    before: impl FnOnce(&XdgToplevel),
) -> usize {
    let index = client.open(app_id, before);
    pump(session, client, |_, client| {
        client.seen.toplevels[index].configured
    });
    let (w, h) = client.seen.toplevels[index]
        .size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or(fallback);
    client.show(index, w, h);
    let app = app_id.to_owned();
    pump(session, client, |session, _| {
        window(session.compositor(), &app).is_some()
    });
    index
}

fn tab(session: &mut Session, client: &mut TestClient, app_id: &str) -> usize {
    map(session, client, app_id, (300, 200), |_| {})
}

/// The window of `app` and its rectangle `(x, y, w, h)`.
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, rect)| (info.id, rect))
}

fn rect(session: &mut Session, app: &str) -> (i32, i32, i32, i32) {
    window(session.compositor(), app).expect("mapped").1
}

/// Which display (0 left, 1 right) the window of `app` starts on; `None`
/// while it is out of sight.
fn display_of(session: &mut Session, app: &str) -> Option<i32> {
    let (x, y, _, _) = rect(session, app);
    (x >= 0 && y >= 0).then(|| x.div_euclid(WIDTH))
}

fn find(resources: &[ResourceSpec], hit: &dyn Fn(&ResourceSpec) -> bool) -> Vec<ResourceSpec> {
    resources
        .iter()
        .flat_map(|resource| {
            let mut found = find(&resource.children, hit);
            if hit(resource) {
                found.insert(0, resource.clone());
            }
            found
        })
        .collect()
}

/// The displays the desk publishes, in reading order.
fn displays(session: &Session) -> Vec<ResourceSpec> {
    find(&session.resources(), &|resource| {
        resource.kind == DISPLAY_KIND
    })
}

fn window_tabs(session: &Session) -> Vec<String> {
    find(&session.resources(), &|resource| {
        resource.kind == WINDOW_KIND
    })
    .into_iter()
    .map(|resource| resource.label)
    .collect()
}

/// The name the desk gives a window whose bar says its own app id.
fn desk_name(app: &str) -> String {
    tilo_desktop_search::names::window_name(app, app)
}

/// Invoke `action` of `resource`, then let the session run a few frames.
fn invoke(
    session: &mut Session,
    client: &mut TestClient,
    resource: &str,
    action: &str,
    arguments: serde_json::Value,
) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    session
        .invoke(resource, action, &arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
    for _ in 0..4 {
        client.exchange();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

fn focus_display(session: &mut Session, client: &mut TestClient, index: usize) {
    let display = displays(session)[index].id.clone();
    invoke(
        session,
        client,
        display.as_str(),
        "focus-display",
        serde_json::json!({}),
    );
}

/// The strip each display shows, by display id.
fn shown_strips(session: &mut Session) -> Vec<(u32, String)> {
    session.desktop().with_policy(|policy| {
        policy.handle().read(|tree| {
            tree.outputs
                .displays
                .iter()
                .map(|display| {
                    let strip = tree.strip_shown_on(display.id);
                    (display.id, format!("{strip:?}"))
                })
                .collect()
        })
    })
}

/// Each strip group's width as the model holds it, with the display it lives
/// on and the window it shows: a display's windows keep their width whatever
/// opens on another.
fn widths(session: &mut Session) -> Vec<(u32, WindowWidth, Option<String>)> {
    session.desktop().with_policy(|policy| {
        policy.handle().read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .map(|group| {
                    (
                        tree.outputs.display_of(group.id).unwrap_or(u32::MAX),
                        group.width,
                        group.tabs.first().map(|tab| tab.title.clone()),
                    )
                })
                .collect()
        })
    })
}

fn focused(session: &mut Session) -> Option<String> {
    session
        .compositor()
        .focused_window()
        .and_then(|info| info.app_id)
}

/// How many pixels of `output` show the test windows' colour: what a person
/// sees of the windows on that display.
fn painted(session: &mut Session, output: &str) -> usize {
    session
        .compositor()
        .screenshot(output)
        .expect("a frame")
        .rgba
        .chunks_exact(4)
        .filter(|pixel| pixel[..3] == WINDOW)
        .count()
}

/// Each display keeps the strip it shows: making another strip and stepping
/// strips moves the focused display's own strip, and the other display keeps
/// its strip and its windows throughout.
#[test]
fn each_display_keeps_the_strip_it_shows() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");
    pump(&mut session, &mut client, |session, _| {
        painted(session, "HEADLESS-1") > 0 && painted(session, "HEADLESS-2") > 0
    });

    // The person works on the left display: the new strip shows there, and
    // the right display keeps the strip it shows, window and all.
    focus_display(&mut session, &mut client, 0);
    invoke(
        &mut session,
        &mut client,
        CHROME_RESOURCE,
        "new-strip",
        serde_json::json!({}),
    );
    for _ in 0..60 {
        client.exchange();
        session.step(Duration::from_millis(16)).unwrap();
    }
    let shown = shown_strips(&mut session);
    assert_ne!(
        shown[0].1, shown[1].1,
        "the left display took the new strip and the right kept its own: {shown:?}"
    );
    pump(&mut session, &mut client, |session, _| {
        painted(session, "HEADLESS-1") == 0 && painted(session, "HEADLESS-2") > 0
    });

    // Back to the strip the left display showed, its window is there again.
    invoke(
        &mut session,
        &mut client,
        CHROME_RESOURCE,
        "step-strip",
        serde_json::json!({"steps": -1}),
    );
    pump(&mut session, &mut client, |session, _| {
        painted(session, "HEADLESS-1") > 0 && painted(session, "HEADLESS-2") > 0
    });

    // The person goes on to the right display and steps on there: that
    // display moves to the other strip, the left one stays where it is.
    focus_display(&mut session, &mut client, 1);
    invoke(
        &mut session,
        &mut client,
        CHROME_RESOURCE,
        "step-strip",
        serde_json::json!({"steps": 1}),
    );
    pump(&mut session, &mut client, |session, _| {
        painted(session, "HEADLESS-1") > 0 && painted(session, "HEADLESS-2") == 0
    });

    // And back, each display with its window.
    invoke(
        &mut session,
        &mut client,
        CHROME_RESOURCE,
        "step-strip",
        serde_json::json!({"steps": -1}),
    );
    pump(&mut session, &mut client, |session, _| {
        painted(session, "HEADLESS-1") > 0 && painted(session, "HEADLESS-2") > 0
    });
    assert_eq!(focused(&mut session).as_deref(), Some("org.example.Right"));
}

/// A window opening on one display leaves the windows of the other display
/// the size they are: two displays are two strips' worth of room, so nothing
/// is shared across the seam.
#[test]
fn a_window_opening_beside_one_display_leaves_the_other_alone() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    pump(&mut session, &mut client, |session, _| {
        display_of(session, "org.example.Left") == Some(0)
    });
    let alone = rect(&mut session, "org.example.Left");
    let width = widths(&mut session);
    assert!(
        painted(&mut session, "HEADLESS-1") > 0,
        "the left display shows its window"
    );

    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");
    pump(&mut session, &mut client, |session, _| {
        display_of(session, "org.example.Right") == Some(1)
    });
    assert_eq!(
        widths(&mut session).first(),
        width.first(),
        "the left display's window keeps the width it is given"
    );
    assert_eq!(
        rect(&mut session, "org.example.Left"),
        alone,
        "and the place and size a person sees it at"
    );
    assert!(
        painted(&mut session, "HEADLESS-2") > 0,
        "the window that opened shows on its own display"
    );
}

#[test]
fn new_windows_open_on_the_display_with_focus() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    let names: Vec<String> = displays(&session).into_iter().map(|d| d.label).collect();
    assert_eq!(
        names.len(),
        2,
        "the desk publishes both displays: {names:?}"
    );

    tab(&mut session, &mut client, "org.example.Left");
    assert_eq!(display_of(&mut session, "org.example.Left"), Some(0));

    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");
    pump(&mut session, &mut client, |session, _| {
        display_of(session, "org.example.Right") == Some(1)
    });
    assert_eq!(
        display_of(&mut session, "org.example.Left"),
        Some(0),
        "the first window stays on its display"
    );
    assert_eq!(focused(&mut session).as_deref(), Some("org.example.Right"));
}

/// The displays' own arrangement action moves the pictures: mirrored, the
/// display beside the one with the window shows that window too.
#[test]
fn an_arrangement_action_mirrors_the_other_display() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    assert_eq!(
        painted(&mut session, "HEADLESS-2"),
        0,
        "the display beside the window shows nothing yet"
    );

    invoke(
        &mut session,
        &mut client,
        tilo_session::displays::RESOURCE,
        "set-mode",
        serde_json::json!({"mode": "mirror"}),
    );
    pump(&mut session, &mut client, |session, _| {
        painted(session, "HEADLESS-2") > 0
    });
    assert_eq!(
        painted(&mut session, "HEADLESS-2"),
        painted(&mut session, "HEADLESS-1"),
        "a mirror shows both displays the same picture"
    );
}

#[test]
fn focus_follows_the_pointer_onto_the_other_display() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");
    assert_eq!(focused(&mut session).as_deref(), Some("org.example.Right"));

    let (x, y, w, h) = rect(&mut session, "org.example.Left");
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: f64::from(x + w / 2),
            y: f64::from(y + h / 2),
        })
        .unwrap();
    pump(&mut session, &mut client, |session, _| {
        focused(session).as_deref() == Some("org.example.Left")
    });

    // Moving about the same display leaves focus where it is.
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: f64::from(x + 1),
            y: f64::from(y + 1),
        })
        .unwrap();
    for _ in 0..4 {
        client.exchange();
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert_eq!(focused(&mut session).as_deref(), Some("org.example.Left"));
}

#[test]
fn focus_steps_across_displays_and_a_window_moves_to_the_next() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");

    invoke(
        &mut session,
        &mut client,
        DESK_RESOURCE,
        "step-focus",
        serde_json::json!({"steps": -1}),
    );
    pump(&mut session, &mut client, |session, _| {
        focused(session).as_deref() == Some("org.example.Left")
    });

    invoke(
        &mut session,
        &mut client,
        DESK_RESOURCE,
        "move-to-display",
        serde_json::json!({"steps": 1}),
    );
    pump(&mut session, &mut client, |session, _| {
        display_of(session, "org.example.Left") == Some(1)
    });
    assert_eq!(display_of(&mut session, "org.example.Right"), Some(1));
    assert_eq!(focused(&mut session).as_deref(), Some("org.example.Left"));
}

#[test]
fn a_mirror_is_one_display() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0), (0, 0)]);
    let mut client = client(&mut session);
    assert!(
        displays(&session).is_empty(),
        "one display needs no telling apart"
    );
    tab(&mut session, &mut client, "org.example.Only");
    assert_eq!(display_of(&mut session, "org.example.Only"), Some(0));
    assert_eq!(window_tabs(&session), [desk_name("org.example.Only")]);
}

#[test]
fn a_dialog_floats_centred_over_its_parent_without_a_tab() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    let parent = tab(&mut session, &mut client, "org.example.Editor");
    let parent_toplevel = client.windows[parent].1.clone();
    map(
        &mut session,
        &mut client,
        "org.example.Editor.Save",
        (320, 180),
        |toplevel| toplevel.set_parent(Some(&parent_toplevel)),
    );

    let (px, py, pw, ph) = rect(&mut session, "org.example.Editor");
    let (dx, dy, dw, dh) = rect(&mut session, "org.example.Editor.Save");
    assert_eq!((dw, dh), (320, 180), "a dialog keeps the size it chose");
    assert!(
        (dx - (px + (pw - dw) / 2)).abs() <= 1 && (dy - (py + (ph - dh) / 2)).abs() <= 1,
        "centred over its parent: parent {:?}, dialog {:?}",
        (px, py, pw, ph),
        (dx, dy, dw, dh)
    );
    assert_eq!(
        window_tabs(&session),
        [desk_name("org.example.Editor")],
        "a dialog takes no tab"
    );
    assert_eq!(
        focused(&mut session).as_deref(),
        Some("org.example.Editor.Save"),
        "the dialog has the keyboard"
    );
    let info = session
        .compositor()
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some("org.example.Editor.Save"))
        .unwrap()
        .0;
    assert!(info.parent.is_some(), "the compositor tells its parent");
}

#[test]
fn a_fixed_size_window_floats_on_the_display_with_focus() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    focus_display(&mut session, &mut client, 1);
    map(
        &mut session,
        &mut client,
        "org.example.Prompt",
        (400, 240),
        |toplevel| {
            toplevel.set_min_size(400, 240);
            toplevel.set_max_size(400, 240);
        },
    );
    let (x, y, w, h) = rect(&mut session, "org.example.Prompt");
    assert_eq!((w, h), (400, 240));
    assert_eq!(
        (x, y),
        (WIDTH + (WIDTH - w) / 2, (HEIGHT - h) / 2),
        "centred on the focused display"
    );
    assert!(window_tabs(&session).is_empty(), "no tab for a dialog");
}

#[test]
fn a_question_opens_on_the_display_of_the_window_in_focus() {
    let _settled = ui::motion::testing::settled();
    let questions = tilo_service::Questions::new();
    let outputs = [
        OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT).at(0, 0),
        OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT).at(WIDTH, 0),
    ];
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            questions: Some(questions.clone()),
            ..Options::default()
        },
        &outputs,
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");
    pump(&mut session, &mut client, |session, _| {
        focused(session).as_deref() == Some("org.example.Right")
    });

    // Held for the whole test: dropping it withdraws the question.
    let _asked = questions.ask(tilo_service::Question {
        handle: "display-test:/access".to_owned(),
        kind: "portal.access".to_owned(),
        app_id: Some("org.example.Counter".to_owned()),
        label: "org.example.Counter".to_owned(),
        details: serde_json::json!({
            "capability": "screencast",
            "reason": null,
            "may_remember": false,
            "text": null,
        }),
    });
    let right = (
        WIDTH,
        0,
        u32::try_from(WIDTH).unwrap(),
        u32::try_from(HEIGHT).unwrap(),
    );
    pump(&mut session, &mut client, |session, _| {
        let presentation = session.portal().expect("portal sheet").presentation();
        presentation.place().is_some() && presentation.is_open()
    });
    assert_eq!(
        session
            .portal()
            .expect("portal sheet")
            .presentation()
            .place(),
        Some(right),
        "the sheet covers the display of the window in focus"
    );
    assert!(
        session
            .portal()
            .expect("portal sheet")
            .presentation()
            .is_open(),
        "and is up for the person"
    );
}

impl Dispatch<WlRegistry, ()> for Seen {
    fn event(
        seen: &mut Self,
        registry: &WlRegistry,
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
        {
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Seen {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, usize> for Seen {
    fn event(
        seen: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            if let Some(toplevel) = seen.toplevels.get_mut(*index) {
                toplevel.configured = true;
            }
        }
    }
}

impl Dispatch<XdgToplevel, usize> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event
            && let Some(toplevel) = seen.toplevels.get_mut(*index)
        {
            toplevel.size = Some((width, height));
        }
    }
}

delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
