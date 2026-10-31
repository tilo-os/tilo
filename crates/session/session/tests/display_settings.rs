//! The person's settings reaching the compositor, and what the compositor
//! reports reaching Settings: their arrangement of the displays applied and
//! kept, a change that could darken a screen waiting for their answer and
//! being put back when it does not come, and the keyboard typing in the
//! input source they chose.

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

use std::time::Duration;

use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
use settings_store::hardware::{
    DisplayConfig, DisplaySet, DisplayTransaction, DisplayTransactionState, OutputConfig,
    OutputMode, OutputTransform,
};
use tilo_compositor::{InputEvent, OutputConfig as CompositorOutput, Screenshot};
use tilo_session::{Options, Session, SessionConfig};
use tilo_sheets::Seat as _;
use tilo_sheets::display_confirm::{self, Question};
use wayland_client::protocol::{wl_registry, wl_registry::WlRegistry};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, delegate_noop, event_created_child,
};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, EVT_HEAD_OPCODE, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// What a key sends, evdev codes.
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;

/// A session with `count` displays side by side.
fn session(count: usize) -> Session {
    let outputs: Vec<CompositorOutput> = (1..=count)
        .map(|index| {
            CompositorOutput::new(format!("HEADLESS-{index}"), WIDTH, HEIGHT)
                .at(WIDTH * i32::try_from(index - 1).unwrap(), 0)
        })
        .collect();
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            ..Options::default()
        },
        &outputs,
    )
    .expect("session")
}

/// The monitors attached now, as Settings sees them, and the set they make.
fn attached(session: &mut Session) -> (Vec<String>, DisplaySet) {
    let snapshot = session
        .compositor()
        .display_snapshot()
        .cloned()
        .expect("the displays are published");
    let descriptors: Vec<_> = snapshot
        .outputs
        .iter()
        .map(|output| output.descriptor.clone())
        .collect();
    let connectors = descriptors
        .iter()
        .map(|descriptor| descriptor.connector.clone())
        .collect();
    (connectors, DisplaySet::of_descriptors(&descriptors))
}

/// Whether the display `connector` is on.
fn lit(session: &mut Session, connector: &str) -> bool {
    session
        .compositor()
        .outputs()
        .into_iter()
        .find(|output| output.name == connector)
        .is_none_or(|output| output.enabled)
}

/// The scale clients render on the display `connector` at; `None` when no
/// display says.
fn scale(session: &mut Session, connector: &str) -> Option<f64> {
    session
        .compositor()
        .display_snapshot()
        .and_then(|snapshot| {
            snapshot
                .outputs
                .iter()
                .find(|output| output.descriptor.connector == connector)
                .map(|output| output.scale)
        })
}

/// Let the session run until `done` holds.
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    for _ in 0..500 {
        session.step(Duration::from_millis(2)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached");
}

/// The question standing on the person's displays now, if one does.
fn asking(session: &Session) -> Option<Question> {
    session
        .display_confirm()
        .filter(|seat| seat.presentation().is_drawn())
        .map(tilo_sheets::display_confirm::ConfirmSeat::question)
}

/// Press and release `keycode` on the session's keyboard.
fn tap(session: &mut Session, keycode: u32) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key { keycode, pressed })
            .expect("the key");
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// A frame of `output`, once the session has drawn what it has.
fn shot(session: &mut Session, output: &str) -> Screenshot {
    session
        .compositor()
        .screenshot(output)
        .unwrap_or_else(|error| panic!("a frame of {output}: {error}"))
}

fn pixel(frame: &Screenshot, x: i32, y: i32) -> [u8; 4] {
    let x = u32::try_from(x).expect("on the display");
    let y = u32::try_from(y).expect("on the display");
    frame.pixel(x, y).expect("a pixel of the frame")
}

/// How many pixels `after` has that `before` does not, and the rectangle
/// they make.
fn changed(before: &Screenshot, after: &Screenshot) -> (usize, (u32, u32, u32, u32)) {
    let mut count = 0;
    let mut bounds = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..after.height {
        for x in 0..after.width {
            if before.pixel(x, y) != after.pixel(x, y) {
                count += 1;
                bounds = (
                    bounds.0.min(x),
                    bounds.1.min(y),
                    bounds.2.max(x),
                    bounds.3.max(y),
                );
            }
        }
    }
    (count, bounds)
}

/// Stage a change through Settings that turns `right` off and keeps `left`
/// where it is: the one that could leave the person with a dark screen.
fn stage_turning_off(set: &DisplaySet, left: &str, right: &str) {
    let mut candidate = DisplayConfig::default();
    candidate.outputs.insert(
        left.to_owned(),
        OutputConfig {
            x: 0,
            y: 0,
            ..OutputConfig::default()
        },
    );
    candidate.outputs.insert(
        right.to_owned(),
        OutputConfig {
            enabled: false,
            ..OutputConfig::default()
        },
    );
    DisplayTransaction::request(set.clone(), candidate).expect("a change staged");
}

#[test]
fn a_scale_chosen_in_settings_reaches_the_displays_and_is_kept() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session(1);
    let (connectors, set) = attached(&mut session);
    let connector = connectors[0].clone();

    let mut baseline = DisplayConfig::default();
    baseline
        .outputs
        .insert(connector.clone(), OutputConfig::default());
    baseline.save_for(&set).expect("the arrangement so far");

    let mut candidate = DisplayConfig::default();
    candidate.outputs.insert(
        connector.clone(),
        OutputConfig {
            scale: 1.5,
            ..OutputConfig::default()
        },
    );
    DisplayTransaction::request(set.clone(), candidate).expect("a change staged");

    until(&mut session, |session| {
        scale(session, &connector) == Some(1.5)
    });
    assert_eq!(
        DisplayTransaction::load().map(|transaction| transaction.state),
        Some(DisplayTransactionState::Confirmed),
        "a scale cannot darken a screen, so it stays at once"
    );
    assert!(
        (DisplayConfig::load_for(&set).outputs[&connector].scale - 1.5).abs() < f64::EPSILON,
        "the choice outlasts this session"
    );
}

#[test]
fn a_change_that_could_dark_a_screen_waits_and_is_put_back() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session(2);
    let (connectors, set) = attached(&mut session);
    let (left, right) = (connectors[0].clone(), connectors[1].clone());

    let mut candidate = DisplayConfig::default();
    candidate.outputs.insert(
        left.clone(),
        OutputConfig {
            x: 0,
            y: 0,
            ..OutputConfig::default()
        },
    );
    candidate.outputs.insert(
        right.clone(),
        OutputConfig {
            enabled: false,
            ..OutputConfig::default()
        },
    );
    DisplayTransaction::request(set, candidate).expect("a change staged");

    until(&mut session, |session| !lit(session, &right));
    let mut transaction = DisplayTransaction::load().expect("the change is staged");
    assert_eq!(
        transaction.state,
        DisplayTransactionState::AwaitingConfirmation,
        "turning a display off waits for the person"
    );
    let deadline = transaction
        .deadline_unix_ms
        .expect("a deadline to answer by");
    assert!(
        deadline.abs_diff(unix_now_ms()) <= DisplayTransaction::CONFIRM_TIMEOUT_MS,
        "the answer has fifteen seconds"
    );

    // The person does not answer; the deadline passes.
    transaction.deadline_unix_ms = Some(unix_now_ms());
    transaction.save().expect("the deadline moved");
    until(&mut session, |session| lit(session, &right));
    assert_eq!(
        DisplayTransaction::load().map(|transaction| transaction.state),
        Some(DisplayTransactionState::Reverted),
        "the previous arrangement is back"
    );
    assert!(
        lit(&mut session, &left),
        "the screen in use never went dark"
    );
}

#[test]
fn a_change_that_could_dark_a_screen_asks_and_keep_leaves_it() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session(2);
    let (connectors, set) = attached(&mut session);
    let (left, right) = (connectors[0].clone(), connectors[1].clone());
    stage_turning_off(&set, &left, &right);

    // The change applies, and the person is asked about it on the display
    // that is still on, with the time they have left.
    until(&mut session, |session| {
        asking(session).is_some_and(|question| question.displays.len() == 1)
    });
    let asked = asking(&session).expect("the question stands");
    assert_eq!(asked.tool, None, "Settings made the change");
    assert!(!asked.answering);
    assert!(
        (0..=15).any(|seconds| asked.status == display_confirm::status(Some(seconds), false)),
        "the countdown shows: {:?}",
        asked.status
    );
    assert!(!lit(&mut session, &right), "the change applied first");
    assert!(lit(&mut session, &left));

    // Keep leaves the layout as it is.
    tap(&mut session, KEY_ENTER);
    until(&mut session, |session| {
        asking(session).is_none()
            && DisplayTransaction::load().map(|transaction| transaction.state)
                == Some(DisplayTransactionState::Confirmed)
    });
    assert!(!lit(&mut session, &right), "keeping keeps the layout");
    assert!(lit(&mut session, &left));
}

#[test]
fn walking_away_puts_the_previous_layout_back_and_takes_the_question_down() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session(2);
    let (connectors, set) = attached(&mut session);
    let (left, right) = (connectors[0].clone(), connectors[1].clone());
    stage_turning_off(&set, &left, &right);

    until(&mut session, |session| asking(session).is_some());
    assert!(!lit(&mut session, &right), "the change applied first");

    // The person does not answer; the deadline passes.
    let mut transaction = DisplayTransaction::load().expect("the change waits");
    transaction.deadline_unix_ms = Some(unix_now_ms());
    transaction.save().expect("the deadline moved");
    until(&mut session, |session| {
        asking(session).is_none() && lit(session, &right)
    });
    assert_eq!(
        DisplayTransaction::load().map(|transaction| transaction.state),
        Some(DisplayTransactionState::Reverted),
        "the previous layout is back"
    );
    assert!(
        lit(&mut session, &left),
        "the screen in use never went dark"
    );
}

#[test]
fn a_tools_risky_change_asks_on_every_display_and_keeping_it_for_good_is_remembered() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session(2);
    attached(&mut session);
    let mine = DisplayTool::new(
        std::env::current_exe()
            .expect("the test binary")
            .to_string_lossy(),
    );
    let mut tool = Tool::connect(&mut session);
    tool.until_heads(&mut session);

    let before_right = shot(&mut session, "HEADLESS-2");
    // A scale the display in use never showed: risky, so it is asked about.
    tool.rescale(&mut session, "HEADLESS-1", 2.0);

    until(&mut session, |session| {
        asking(session).is_some_and(|question| question.displays.len() == 2)
    });
    let asked = asking(&session).expect("the question stands");
    assert_eq!(
        asked.tool.as_deref(),
        Some(mine.name()),
        "the question names the tool that made the change"
    );
    assert!(lit(&mut session, "HEADLESS-1") && lit(&mut session, "HEADLESS-2"));
    assert!((scale(&mut session, "HEADLESS-1").expect("the display") - 2.0).abs() < f64::EPSILON);

    // The card stands on the display the change did not touch, centred on
    // it: what the person would miss is drawn on every display.
    let card_left = shot(&mut session, "HEADLESS-1");
    let card_right = shot(&mut session, "HEADLESS-2");
    let (_, (x0, y0, x1, y1)) = changed(&before_right, &card_right);
    assert!(x1 > x0 && y1 > y0, "nothing was drawn on it");
    let middle = (
        i64::midpoint(i64::from(x0), i64::from(x1)) - i64::from(WIDTH / 2),
        i64::midpoint(i64::from(y0), i64::from(y1)) - i64::from(HEIGHT / 2),
    );
    assert!(
        middle.0.abs() < 40 && middle.1.abs() < 40,
        "the card sits at the middle of the display: {x0} {y0} {x1} {y1}"
    );

    // Keep and stop asking about this tool: the third answer, after Keep and
    // Revert in the keyboard's order.
    tap(&mut session, KEY_TAB);
    tap(&mut session, KEY_TAB);
    tap(&mut session, KEY_ENTER);
    until(&mut session, |session| asking(session).is_none());
    for _ in 0..5 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    assert!(asking(&session).is_none(), "the question stays down");

    // Answering took the card down on every display: each one's middle
    // shows the desktop again.
    let (centre_x, centre_y) = (WIDTH / 2, HEIGHT / 2);
    for (name, card) in [("HEADLESS-1", &card_left), ("HEADLESS-2", &card_right)] {
        assert_ne!(
            pixel(card, centre_x, centre_y),
            pixel(&shot(&mut session, name), centre_x, centre_y),
            "the card stood in the middle of {name}"
        );
    }
    assert!(
        TrustedDisplayTools::load().contains(&mine),
        "the tool is trusted from now on"
    );
    assert!(
        (scale(&mut session, "HEADLESS-1").expect("the display") - 2.0).abs() < f64::EPSILON,
        "the change is kept"
    );
    assert!(lit(&mut session, "HEADLESS-1") && lit(&mut session, "HEADLESS-2"));
}

/// A display tool on the privileged display socket, speaking
/// `wlr-output-management` to the session's compositor.
struct Tool {
    connection: Connection,
    queue: EventQueue<ToolState>,
    state: ToolState,
}

/// What the tool has heard.
#[derive(Default)]
struct ToolState {
    manager: Option<ZwlrOutputManagerV1>,
    heads: Vec<ToolHead>,
    serial: Option<u32>,
    /// Whether the last configuration applied.
    outcome: Option<bool>,
}

#[derive(Default, Debug)]
struct ToolHead {
    object: Option<ZwlrOutputHeadV1>,
    name: String,
    scale: f64,
}

impl Tool {
    fn connect(session: &mut Session) -> Self {
        let socket = session
            .compositor()
            .connect_trusted_client()
            .expect("a client socket");
        let connection = Connection::from_socket(socket).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            state: ToolState::default(),
        }
    }

    fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.state)
            .expect("client dispatch");
        self.connection.flush().expect("flush");
    }

    /// Let the tool and the session talk until `done` holds.
    #[track_caller]
    fn pump(&mut self, session: &mut Session, mut done: impl FnMut(&ToolState) -> bool) {
        for _ in 0..500 {
            self.exchange();
            session.step(Duration::from_millis(2)).expect("step");
            self.exchange();
            if done(&self.state) {
                return;
            }
        }
        panic!("condition not reached");
    }

    fn until_heads(&mut self, session: &mut Session) {
        self.pump(session, |state| {
            state.serial.is_some() && state.heads.len() == 2
        });
    }

    /// Give the display `name` the scale `scale`, as a layout tool does.
    #[track_caller]
    fn rescale(&mut self, session: &mut Session, name: &str, scale: f64) {
        let head = self
            .state
            .heads
            .iter()
            .find(|head| head.name == name)
            .and_then(|head| head.object.clone())
            .expect("head");
        let handle = self.queue.handle();
        let manager = self.state.manager.clone().expect("manager");
        self.state.outcome = None;
        let configuration =
            manager.create_configuration(self.state.serial.expect("serial"), &handle, ());
        configuration
            .enable_head(&head, &handle, ())
            .set_scale(scale);
        configuration.apply();
        self.pump(session, |state| state.outcome.is_some());
        assert_eq!(self.state.outcome, Some(true), "the compositor takes it");
    }
}

impl Dispatch<WlRegistry, ()> for ToolState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _connection: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
            && interface == "zwlr_output_manager_v1"
        {
            state.manager = Some(registry.bind(name, 4, handle, ()));
        }
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for ToolState {
    fn event(
        state: &mut Self,
        _manager: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        (): &(),
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => state.heads.push(ToolHead {
                object: Some(head),
                ..ToolHead::default()
            }),
            zwlr_output_manager_v1::Event::Done { serial } => state.serial = Some(serial),
            _ => {}
        }
    }

    event_created_child!(ToolState, ZwlrOutputManagerV1, [
        EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for ToolState {
    fn event(
        state: &mut Self,
        object: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        (): &(),
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        let Some(head) = state
            .heads
            .iter_mut()
            .find(|head| head.object.as_ref() == Some(object))
        else {
            return;
        };
        match event {
            zwlr_output_head_v1::Event::Name { name } => head.name = name,
            zwlr_output_head_v1::Event::Scale { scale } => head.scale = scale,
            _ => {}
        }
    }

    event_created_child!(ToolState, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for ToolState {
    fn event(
        _state: &mut Self,
        _mode: &ZwlrOutputModeV1,
        _event: zwlr_output_mode_v1::Event,
        (): &(),
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ()> for ToolState {
    fn event(
        state: &mut Self,
        _configuration: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        (): &(),
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_configuration_v1::Event::Succeeded => state.outcome = Some(true),
            zwlr_output_configuration_v1::Event::Failed
            | zwlr_output_configuration_v1::Event::Cancelled => state.outcome = Some(false),
            _ => {}
        }
    }
}

delegate_noop!(ToolState: ignore ZwlrOutputConfigurationHeadV1);

#[test]
fn the_mode_scale_and_rotation_chosen_in_settings_are_back_after_a_restart() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut first = session(1);
    let (connectors, set) = attached(&mut first);
    let connector = connectors[0].clone();
    let chosen_mode = OutputMode {
        width: 800,
        height: 600,
        refresh_mhz: 75_000,
    };
    let mut chosen = DisplayConfig::default();
    chosen.outputs.insert(
        connector.clone(),
        OutputConfig {
            scale: 1.25,
            transform: OutputTransform::Rotate90,
            mode: Some(chosen_mode),
            ..OutputConfig::default()
        },
    );
    chosen.save_for(&set).expect("the arrangement is kept");
    drop(first);

    // The next session starts from nothing and reads the same choices.
    let mut restarted = session(1);
    until(&mut restarted, |session| {
        scale(session, &connector) == Some(1.25)
    });
    let shown = restarted
        .compositor()
        .display_snapshot()
        .and_then(|snapshot| {
            snapshot
                .outputs
                .iter()
                .find(|output| output.descriptor.connector == connector)
                .map(|output| output.current_mode)
        })
        .expect("the display is published");
    assert_eq!(shown, chosen_mode, "the mode and refresh rate hold");
    let rect = restarted
        .compositor()
        .outputs()
        .into_iter()
        .find(|output| output.name == connector)
        .and_then(|output| output.rect)
        .expect("the display is on");
    assert_eq!(
        (rect.2, rect.3),
        (480, 640),
        "800x600 at 1.25 turned on its side is 480x640: {rect:?}"
    );
}

#[test]
fn the_keyboard_types_in_the_next_input_source_and_keeps_the_choice() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    settings_store::update_section(
        settings_store::hardware::INPUT_SECTION,
        serde_json::Map::from_iter([
            (
                "keyboard_layout".to_owned(),
                serde_json::Value::String("us".into()),
            ),
            (
                "keyboard_layouts".to_owned(),
                serde_json::json!(["us", "de"]),
            ),
        ]),
    )
    .expect("the person's sources are kept");
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            config: SessionConfig::load(),
            ..Options::default()
        },
        &[CompositorOutput::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    assert_eq!(
        session.active_input_source(),
        "us",
        "the person types in the source they chose: {:?}",
        session.input_sources()
    );
    assert_eq!(
        session.compositor().keyboard_layout().as_deref(),
        Some("English (US)")
    );

    session
        .invoke("session:input", "next-source", &serde_json::Map::new())
        .expect("the session publishes the source switch");
    until(&mut session, |session| {
        session.active_input_source() == "de"
    });
    assert_eq!(
        session.compositor().keyboard_layout().as_deref(),
        Some("German"),
        "the seat types in German now"
    );
    assert_eq!(
        settings_store::load_section(settings_store::hardware::INPUT_SECTION)
            .get("keyboard_layout")
            .cloned(),
        Some(serde_json::Value::String("de".into())),
        "the choice survives the session"
    );
}

/// Milliseconds since the Unix epoch.
fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}
