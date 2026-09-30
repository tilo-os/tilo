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

#[test]
fn agents_in_an_empty_workspace_see_the_whole_screen_until_windows_open() {
    let mut fx = Fixture::new();
    let (strip_a, viewport) = fx.seat("a");
    assert_eq!(
        viewport,
        [0.0, 0.0, 800.0, 600.0],
        "no window anywhere: the empty-workspace card is the screen"
    );
    fx.add_window(strip_a);
    let (_, [_, ay, _, ah]) = fx.seat("a");
    assert!(ah > 0.0 && ah <= 600.0 - ay, "a's strip is a row now");
}

#[test]
fn two_agents_get_their_own_strips_stacked_on_screen() {
    let mut fx = Fixture::new();
    let (strip_a, _) = fx.seat("a");
    fx.add_window(strip_a);
    let (_, [_, _, alone_w, alone_h]) = fx.seat("a");
    assert_eq!(fx.active_strip(), Some(strip_a), "a's strip has focus");
    assert!(alone_w > 0.0 && alone_h > 0.0);
    let (strip_b, _) = fx.seat("b");
    assert_ne!(strip_a, strip_b);
    assert_eq!(fx.active_strip(), Some(strip_b), "b's strip has focus");
    assert_eq!(fx.strip_names(), vec!["Agent a", "Agent b"]);
    fx.add_window(strip_b);
    let (_, [_, by, bw, bh]) = fx.seat("b");
    let (again, [_, ay, _, ah]) = fx.seat("a");
    assert_eq!(again, strip_a, "an agent keeps its strip");
    assert_eq!(fx.active_strip(), Some(strip_a));
    assert!(bw > 0.0 && bh > 0.0 && ah > 0.0);
    assert!(
        ay + ah <= by || by + bh <= ay,
        "the strips do not overlap: a {ay}+{ah}, b {by}+{bh}"
    );
    assert!(ah < alone_h, "a's strip shrank to make room for b's");
}

#[test]
fn coordinates_are_relative_to_the_agents_strip() {
    let mut fx = Fixture::new();
    let (strip_a, _) = fx.seat("a");
    fx.add_window(strip_a);
    let (strip_b, _) = fx.seat("b");
    fx.add_window(strip_b);
    let (_, [bx, by, _, bh]) = fx.seat("b");
    assert!(by > 0.0, "b's strip is below a's");
    fx.ok(
        "b",
        serde_json::json!({"op":"pointer_move","x":10.0,"y":20.0}),
    );
    assert_eq!(fx.state.client_input.cursor(), (bx + 10.0, by + 20.0));

    let refused = fx.send(
        "b",
        serde_json::json!({"op":"click_at","x":10.0,"y":bh + 1.0}),
    );
    assert_eq!(refused["err"]["code"], "outside_viewport", "{refused}");
    assert_eq!(
        fx.state.client_input.cursor(),
        (bx + 10.0, by + 20.0),
        "a refused click moves nothing"
    );

    let (_, [ax, ay, _, _]) = fx.seat("a");
    assert_eq!(
        fx.state.client_input.cursor(),
        (bx + 10.0, by + 20.0),
        "a never moved its pointer, so the seat keeps b's"
    );
    fx.ok(
        "a",
        serde_json::json!({"op":"pointer_move","x":5.0,"y":5.0}),
    );
    assert_eq!(fx.state.client_input.cursor(), (ax + 5.0, ay + 5.0));
    let _ = fx.seat("b");
    assert_eq!(
        fx.state.client_input.cursor(),
        (bx + 10.0, by + 20.0),
        "b's turn puts the pointer back where b left it"
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
