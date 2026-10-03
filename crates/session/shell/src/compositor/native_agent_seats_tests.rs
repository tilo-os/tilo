//! Two agents driving one compositor over the agent-control socket at once:
//! each gets its own strip and seat, and neither one's input reaches the
//! other's. Driven through the real transport ([`pump_test_control_conn`])
//! against a live frame and Wayland seat.

use super::*;
use std::io::BufRead as _;

struct Fixture {
    frame_builder: FrameBuilder,
    ctx: TestControlCtx,
    session: SessionManager,
    lock: LockController,
    state: State,
    conn: TestControlConn,
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    _event_loop: EventLoop<'static, State>,
    _display: Display<State>,
}

impl Fixture {
    /// A compositor with one empty workspace, reached over an agent-control
    /// connection.
    fn new() -> Self {
        let mut tree = crate::shell::state::WorkspaceTree::default();
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(91),
            "Agents' machine",
        );
        workspace.owner = Some(crate::shell::state::local_user_id_for_room(&workspace.room));
        tree.desktop.workspaces.push(workspace);
        tree.set_active(crate::shell::state::WorkspaceId(91));
        let frame_builder = FrameBuilder::with_handle(tree.shared(), false, (800, 600));
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let state = State::new(event_loop.handle(), display.handle());
        let (server, client) = UnixStream::pair().expect("socket pair");
        let writer = client.try_clone().expect("client writer");
        let reader = BufReader::new(client);
        Self {
            frame_builder,
            ctx: TestControlCtx::new(),
            session: SessionManager::new(IdleConfig::from_env(), Instant::now()),
            lock: LockController::new(lock_auth::verifier_from_env(), "fixture"),
            state,
            conn: TestControlConn::new(server, true).expect("agent connection"),
            writer,
            reader,
            _event_loop: event_loop,
            _display: display,
        }
    }

    /// Send `command` for `agent` and read the reply.
    fn send(&mut self, agent: &str, mut command: serde_json::Value) -> serde_json::Value {
        command["agent"] = serde_json::json!(agent);
        command["agent_label"] = serde_json::json!(format!("Agent {agent}"));
        self.writer
            .write_all(format!("{command}\n").as_bytes())
            .expect("request");
        pump_test_control_conn(
            &mut self.conn,
            &mut self.frame_builder,
            &mut self.ctx,
            &self.session,
            &mut self.lock,
            &mut self.state,
        );
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("response");
        serde_json::from_str(line.trim_end()).expect("response JSON")
    }

    fn ok(&mut self, agent: &str, command: serde_json::Value) {
        let sent = command.to_string();
        let reply = self.send(agent, command);
        assert!(reply.get("ok").is_some(), "{sent} for {agent}: {reply}");
    }

    /// The agent's strip and its viewport `[x, y, w, h]`.
    fn seat(&mut self, agent: &str) -> (u64, [f64; 4]) {
        let reply = self.send(agent, serde_json::json!({"op":"agent_seat"}));
        let answer = &reply["answer"];
        let strip = answer["strip"].as_u64().expect("strip id");
        let viewport = serde_json::from_value(answer["viewport"].clone()).expect("viewport");
        (strip, viewport)
    }

    fn seat_holds(&self, evdev: u32) -> bool {
        self.state
            .seat
            .get_keyboard()
            .expect("seat keyboard")
            .pressed_keys()
            .iter()
            .any(|code| u32::from(*code) == evdev.saturating_add(8))
    }

    fn active_strip(&self) -> Option<u64> {
        let chrome = self.frame_builder.chrome();
        let tree = chrome.tree();
        tree.desktop
            .active_strip
            .get(&tree.desktop.active)
            .map(|strip| strip.0)
    }

    /// Open a window in `strip`, as a launch the agent made would.
    fn add_window(&mut self, strip: u64) {
        use crate::shell::state::{AppKind, Tab, TileContent, WindowGroup, WindowWidth, next_id};
        self.frame_builder.with_chrome(|chrome| {
            {
                let mut tree = chrome.tree_mut();
                let strip = tree
                    .desktop
                    .active_workspace_mut()
                    .and_then(|ws| ws.strips.iter_mut().find(|s| s.id.0 == strip))
                    .expect("the agent's strip");
                let tab = Tab {
                    id: next_id(),
                    title: "Terminal".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Terminal,
                        label: "Terminal".into(),
                    },
                };
                strip.window_groups.push(WindowGroup {
                    id: next_id(),
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Normal,
                    maximized: false,
                });
            }
            chrome.bump();
        });
        self.frame_builder.relayout();
    }

    /// Open `app_id`'s window, a Wayland client's tab, in `strip`.
    fn add_app_window(&mut self, strip: u64, app_id: &str, surface_id: u64) {
        use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth, next_id};
        self.frame_builder.with_chrome(|chrome| {
            {
                let mut tree = chrome.tree_mut();
                let strip = tree
                    .desktop
                    .active_workspace_mut()
                    .and_then(|ws| ws.strips.iter_mut().find(|s| s.id.0 == strip))
                    .expect("the strip");
                let tab = Tab {
                    id: next_id(),
                    title: app_id.into(),
                    tile: TileContent::Foreign {
                        app_id: app_id.into(),
                        surface_id,
                    },
                };
                strip.window_groups.push(WindowGroup {
                    id: next_id(),
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Normal,
                    maximized: false,
                });
            }
            chrome.bump();
        });
        self.frame_builder.relayout();
    }

    /// Where `strip`'s row is drawn, `None` while it is collapsed.
    fn row(&self, strip: u64) -> Option<Rect> {
        self.frame_builder
            .host()
            .strip_row_rect(crate::shell::state::StripId(strip))
    }

    fn strip_names(&self) -> Vec<String> {
        let chrome = self.frame_builder.chrome();
        let tree = chrome.tree();
        tree.desktop
            .active_workspace()
            .map(|ws| ws.strips.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default()
    }
}

const SHIFT: u32 = 42;
const CONTROL: u32 = 29;

const SCREEN: [f64; 4] = [0.0, 0.0, 800.0, 600.0];

#[test]
fn a_lone_agent_sees_the_whole_screen_before_and_after_it_opens_a_window() {
    let mut fx = Fixture::new();
    let (strip_a, viewport) = fx.seat("a");
    assert_eq!(
        viewport, SCREEN,
        "no window anywhere: the empty-workspace card"
    );
    fx.add_window(strip_a);
    let (_, viewport) = fx.seat("a");
    assert_eq!(
        viewport, SCREEN,
        "its window's row, the top bar and anything opened over them"
    );
    // A strip another agent left behind still exists, but it does not take
    // half of a's screen.
    fx.frame_builder.with_chrome(|chrome| {
        let _ = chrome.agent_strip(None, |_| true, "Someone else");
    });
    fx.frame_builder.relayout();
    let _ = fx.seat("a");
    let row = fx.row(strip_a).expect("a's row is drawn");
    assert!(
        row.height() > 600.0 * 0.75,
        "a's strip fills the work area: {row:?}"
    );
}

#[test]
fn each_agents_strip_fills_the_screen_on_its_turn() {
    let mut fx = Fixture::new();
    let (strip_a, _) = fx.seat("a");
    fx.add_window(strip_a);
    let _ = fx.seat("a");
    let alone = fx.row(strip_a).expect("a's row");
    let (strip_b, viewport) = fx.seat("b");
    assert_ne!(strip_a, strip_b);
    assert_eq!(viewport, SCREEN);
    assert_eq!(fx.active_strip(), Some(strip_b), "b's strip has focus");
    assert_eq!(fx.strip_names(), vec!["Agent a", "Agent b"]);
    fx.add_window(strip_b);
    let _ = fx.seat("b");
    assert_eq!(fx.row(strip_a), None, "a's windows are not on b's screen");
    let b_row = fx.row(strip_b).expect("b's row");
    assert_eq!(b_row, alone, "b's strip gets the screen a had alone");

    let (again, viewport) = fx.seat("a");
    assert_eq!(again, strip_a, "an agent keeps its strip");
    assert_eq!(viewport, SCREEN);
    assert_eq!(fx.active_strip(), Some(strip_a));
    assert_eq!(fx.row(strip_b), None);
    assert_eq!(fx.row(strip_a), Some(alone), "nor did a's shrink");
}

#[test]
fn coordinates_are_screen_pixels_and_each_agent_keeps_its_pointer() {
    let mut fx = Fixture::new();
    let (strip_a, _) = fx.seat("a");
    fx.add_window(strip_a);
    let (strip_b, _) = fx.seat("b");
    fx.add_window(strip_b);
    fx.ok(
        "b",
        serde_json::json!({"op":"pointer_move","x":10.0,"y":20.0}),
    );
    assert_eq!(fx.state.client_input.cursor(), (10.0, 20.0));

    let refused = fx.send("b", serde_json::json!({"op":"click_at","x":10.0,"y":601.0}));
    assert_eq!(refused["err"]["code"], "outside_viewport", "{refused}");
    assert_eq!(
        fx.state.client_input.cursor(),
        (10.0, 20.0),
        "a refused click moves nothing"
    );

    let _ = fx.seat("a");
    assert_eq!(
        fx.state.client_input.cursor(),
        (10.0, 20.0),
        "a never moved its pointer, so the seat keeps b's"
    );
    fx.ok(
        "a",
        serde_json::json!({"op":"pointer_move","x":5.0,"y":5.0}),
    );
    assert_eq!(fx.state.client_input.cursor(), (5.0, 5.0));
    let _ = fx.seat("b");
    assert_eq!(
        fx.state.client_input.cursor(),
        (10.0, 20.0),
        "b's turn puts the pointer back where b left it"
    );
}

#[test]
fn an_agent_that_moves_to_a_free_strip_stays_there() {
    let mut fx = Fixture::new();
    let (strip_a, _) = fx.seat("a");
    fx.add_window(strip_a);
    let mut other = None;
    fx.frame_builder.with_chrome(|chrome| {
        other = chrome.agent_strip(None, |_| true, "Earlier work");
    });
    let other = other.expect("a second strip").0;
    fx.add_window(other);
    let _ = fx.seat("a");
    assert_eq!(fx.active_strip(), Some(strip_a));

    // a switches strips itself, as Super+Down does.
    fx.frame_builder.with_chrome(|chrome| {
        let _ = chrome.focus_strip(crate::shell::state::StripId(other));
    });
    let (strip, _) = fx.seat("a");
    assert_eq!(strip, other, "its next command does not pull it back");
    assert_eq!(fx.active_strip(), Some(other));
    assert!(fx.row(other).is_some() && fx.row(strip_a).is_none());

    // b may not take the strip a works in.
    let (strip_b, _) = fx.seat("b");
    assert_ne!(strip_b, other);
    fx.frame_builder.with_chrome(|chrome| {
        let _ = chrome.focus_strip(crate::shell::state::StripId(other));
    });
    let (strip, _) = fx.seat("b");
    assert_eq!(strip, strip_b, "a's strip stays a's");
    assert_eq!(fx.active_strip(), Some(strip_b));
}

#[test]
fn the_last_agent_leaving_shows_every_strip_again() {
    let mut fx = Fixture::new();
    let (strip_a, _) = fx.seat("a");
    fx.add_window(strip_a);
    let (strip_b, _) = fx.seat("b");
    fx.add_window(strip_b);
    let _ = fx.seat("b");
    assert!(fx.frame_builder.chrome().single_strip());
    fx.ok("a", serde_json::json!({"op":"agent_leave"}));
    assert!(fx.frame_builder.chrome().single_strip(), "b still drives");
    fx.ok("b", serde_json::json!({"op":"agent_leave"}));
    fx.frame_builder.relayout();
    assert!(!fx.frame_builder.chrome().single_strip());
    let (a, b) = (fx.row(strip_a), fx.row(strip_b));
    assert!(
        a.is_some() && b.is_some(),
        "both rows are drawn again: {a:?} {b:?}"
    );
}

#[test]
fn a_held_modifier_never_reaches_the_other_agent() {
    let mut fx = Fixture::new();
    fx.ok("a", serde_json::json!({"op":"key_down","key":"Shift"}));
    assert!(fx.seat_holds(SHIFT), "a's Shift is down on the shared seat");
    assert!(fx.ctx.modifiers.state().shift_key());

    fx.ok("b", serde_json::json!({"op":"key_down","key":"x"}));
    assert!(
        !fx.seat_holds(SHIFT),
        "while b types, the seat holds no Shift of a's"
    );
    assert!(
        !fx.ctx.modifiers.state().shift_key(),
        "nor does the chrome's modifier mask"
    );
    fx.ok("b", serde_json::json!({"op":"key_up","key":"x"}));

    fx.ok("a", serde_json::json!({"op":"key_down","key":"z"}));
    assert!(fx.seat_holds(SHIFT), "a's Shift is back for a's own keys");
    assert!(fx.ctx.modifiers.state().shift_key());
    fx.ok("a", serde_json::json!({"op":"key_up","key":"z"}));
    fx.ok("a", serde_json::json!({"op":"key_up","key":"Shift"}));
    assert!(!fx.seat_holds(SHIFT));
    assert_eq!(fx.state.remote_seats.key_holder_count(SHIFT), 0);
}

#[test]
fn one_agent_cannot_release_what_another_holds() {
    let mut fx = Fixture::new();
    fx.ok("a", serde_json::json!({"op":"key_down","key":"Control"}));
    // b sends a release for a key it never pressed.
    fx.ok("b", serde_json::json!({"op":"key_up","key":"Control"}));
    // Both hold Shift at once: the shared seat sees one hold per agent turn,
    // and b letting go leaves a's hold for a's next turn.
    fx.ok("b", serde_json::json!({"op":"key_down","key":"Shift"}));
    fx.ok("a", serde_json::json!({"op":"key_down","key":"Shift"}));
    assert!(
        fx.seat_holds(CONTROL),
        "b's stray release did not end a's Control"
    );
    assert!(fx.seat_holds(SHIFT));
    fx.ok("b", serde_json::json!({"op":"key_up","key":"Shift"}));
    assert!(!fx.seat_holds(CONTROL), "a is not seated while b acts");
    fx.ok(
        "a",
        serde_json::json!({"op":"pointer_move","x":1.0,"y":1.0}),
    );
    assert!(fx.seat_holds(CONTROL));
    assert!(fx.seat_holds(SHIFT), "b's release did not end a's Shift");
    assert_eq!(fx.state.remote_seats.key_holder_count(SHIFT), 1);
}

#[test]
fn leaving_releases_the_agents_holds_and_its_empty_strip() {
    let mut fx = Fixture::new();
    let (strip_a, _) = fx.seat("a");
    let (strip_b, _) = fx.seat("b");
    fx.ok("b", serde_json::json!({"op":"key_down","key":"Shift"}));
    fx.ok("b", serde_json::json!({"op":"agent_leave"}));
    assert!(!fx.seat_holds(SHIFT), "b's Shift went with b");
    assert_eq!(fx.state.remote_seats.key_holder_count(SHIFT), 0);
    assert_eq!(
        fx.strip_names(),
        vec!["Agent a"],
        "b's empty strip went too"
    );
    assert_eq!(fx.active_strip(), Some(strip_a));
    let (rejoined, _) = fx.seat("b");
    assert_ne!(rejoined, strip_a, "b coming back gets a strip of its own");
    assert_ne!(rejoined, strip_b, "a fresh one: the old one is gone");
}

#[test]
fn agent_only_commands_need_an_agent() {
    let mut fx = Fixture::new();
    // The agent socket fills in its default agent for a line naming none.
    fx.writer
        .write_all(b"{\"op\":\"agent_seat\"}\n")
        .expect("request");
    pump_test_control_conn(
        &mut fx.conn,
        &mut fx.frame_builder,
        &mut fx.ctx,
        &fx.session,
        &mut fx.lock,
        &mut fx.state,
    );
    let mut line = String::new();
    fx.reader.read_line(&mut line).expect("response");
    let reply: serde_json::Value = serde_json::from_str(line.trim_end()).expect("json");
    assert_eq!(reply["answer"]["agent"], DEFAULT_AGENT, "{reply}");

    let bad = fx.send("", serde_json::json!({"op":"key","key":"a"}));
    assert_eq!(bad["err"]["code"], "invalid_agent", "{bad}");
}

#[test]
fn a_locked_session_says_so_instead_of_seating() {
    let mut fx = Fixture::new();
    let (strip_a, _) = fx.seat("a");
    let _ = fx.session.request_lock(Instant::now());
    let refused = fx.send("b", serde_json::json!({"op":"agent_seat"}));
    assert_eq!(refused["err"]["code"], "session_locked", "{refused}");
    assert_eq!(
        fx.active_strip(),
        Some(strip_a),
        "nothing behind the lock screen moved"
    );
}

#[test]
fn a_crop_keeps_exactly_the_viewport_pixels() {
    // 4x3 frame whose red channel is the pixel index.
    let frame = (0u8..12).flat_map(|i| [i, 0, 0, 255]).collect::<Vec<_>>();
    let (w, h, rgba) = crop_rgba(&frame, (4, 3), Rect::new(1.0, 1.0, 3.0, 3.0));
    assert_eq!((w, h), (2, 2));
    let red = rgba.chunks(4).map(|pixel| pixel[0]).collect::<Vec<_>>();
    assert_eq!(red, vec![5, 6, 9, 10]);
    let (w, h, _) = crop_rgba(&frame, (4, 3), Rect::new(3.0, 2.0, 9.0, 9.0));
    assert_eq!((w, h), (1, 1), "clamped to the frame");
}

#[test]
fn an_agent_aimed_at_a_window_acts_in_its_pixels() {
    const SURFACE: u64 = 0x00A9_E17A;
    let mut fx = Fixture::new();
    let (strip_a, _) = fx.seat("a");
    fx.add_app_window(strip_a, "so.example.paint", SURFACE);
    let placed = foreign::store()
        .placement(SURFACE)
        .expect("the window is laid out");

    let reply = fx.send(
        "a",
        serde_json::json!({"op":"agent_seat","window":"so.example.paint"}),
    );
    let answer = &reply["answer"];
    assert_eq!(answer["window"], "so.example.paint", "{reply}");
    let viewport: [f64; 4] = serde_json::from_value(answer["viewport"].clone()).expect("viewport");
    let shown = foreign::store().placement(SURFACE).expect("still laid out");
    assert_eq!(
        viewport,
        [shown.x0, shown.y0, shown.width(), shown.height()],
        "the window's own rect, not the screen (it was {placed:?} before)"
    );
    assert!(viewport[0] > 0.0 || viewport[1] > 0.0, "{viewport:?}");

    fx.ok(
        "a",
        serde_json::json!({"op":"pointer_move","x":3.0,"y":4.0,"window":"so.example.paint"}),
    );
    assert_eq!(
        fx.state.client_input.cursor(),
        (viewport[0] + 3.0, viewport[1] + 4.0),
        "coordinates count from the window's top-left"
    );
    let outside = fx.send(
        "a",
        serde_json::json!({"op":"click_at","x":viewport[2] + 1.0,"y":1.0,"window":"so.example.paint"}),
    );
    assert_eq!(outside["err"]["code"], "outside_viewport", "{outside}");
    assert!(
        outside["err"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("so.example.paint's window")),
        "{outside}"
    );

    let unknown = fx.send(
        "a",
        serde_json::json!({"op":"agent_seat","window":"so.example.missing"}),
    );
    assert_eq!(unknown["err"]["code"], "unknown_window", "{unknown}");
    assert!(
        unknown["err"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("so.example.paint")),
        "the refusal names the windows there are: {unknown}"
    );

    let busy = fx.send(
        "b",
        serde_json::json!({"op":"agent_seat","window":"so.example.paint"}),
    );
    assert_eq!(
        busy["err"]["code"], "window_busy",
        "a's strip stays a's: {busy}"
    );
    fx.ok("a", serde_json::json!({"op":"agent_leave"}));
    let reply = fx.send(
        "b",
        serde_json::json!({"op":"agent_seat","window":"so.example.paint"}),
    );
    let strip_b = reply["answer"]["strip"].as_u64();
    assert_eq!(
        strip_b,
        Some(strip_a),
        "once a left, b may work in its window"
    );
}
