//! An agent's own seat at the control door: its pointer, its keys, the window
//! it aims at and the frames it shows there, none of the person's desk.

use super::{
    BTreeMap, Compositor, Context, Desk, InputEvent, Json, Op, RefCell, button, drag_step, json,
    key_code, number, refusal,
};

/// One agent's seat in the compositor, and the window it was aimed at.
pub(super) struct AgentSeat {
    pub(super) seat: tilo_compositor::Seat,
    pub(super) window: Option<String>,
}

/// The answer to what `op` asks of an agent's own seat, when a seat's connection asks it: its
/// pointer, its keys, the window it aims at and the frames it shows there. `None` for the
/// connections of the other doors.
pub(super) fn asked_of_seat(
    op: Op,
    request: &Json,
    ctx: &Context,
    compositor: &mut Compositor,
    desk: &Desk<'_>,
    seats: &RefCell<BTreeMap<String, AgentSeat>>,
) -> Option<Json> {
    let agent = ctx.seat.as_deref()?;
    // The person's Stop ends the agent's seat under it; a connection left open acts nothing
    // after that.
    if !desk.seats.admits(agent) {
        if let Some(held) = seats.borrow_mut().remove(agent) {
            compositor.close_seat(held.seat);
        }
        return Some(refusal(
            "not_admitted",
            "the person has not admitted this agent; its seat acts nothing",
        ));
    }
    if op == Op::AgentLeave {
        if let Some(held) = seats.borrow_mut().remove(agent) {
            compositor.close_seat(held.seat);
        }
        return Some(json!({"ok": {}}));
    }
    // Every command names the window it acts in, when it names one.
    let window = request.get("window").and_then(Json::as_str);
    let seat = match seat_of(compositor, seats, agent, window) {
        Ok(seat) => seat,
        Err(message) => return Some(refusal("no_window", &message)),
    };
    Some(seat_op(op, request, agent, window, seat, compositor))
}

/// What `op` does at `seat`, the seat of `agent` aimed at `window`.
fn seat_op(
    op: Op,
    request: &Json,
    agent: &str,
    window: Option<&str>,
    seat: tilo_compositor::Seat,
    compositor: &mut Compositor,
) -> Json {
    match op {
        Op::AgentSeat => {
            let Some(aim) = compositor.seat_view(seat) else {
                return refusal("no_seat", "this seat has ended");
            };
            let (x, y, width, height) = aim.viewport();
            json!({"answer": {"agent": agent, "window": window, "viewport": [x, y, width, height]}})
        }
        Op::Snapshot => seat_snapshot(request, seat, compositor),
        Op::PointerMove => {
            let (Some(x), Some(y)) = (number(request, "x"), number(request, "y")) else {
                return refusal("parse_error", "`pointer_move` takes `x` and `y`");
            };
            let (x, y) = seat_point(compositor, seat, x, y);
            seat_answer(compositor.seat_input(seat, &InputEvent::PointerMotion { x, y }))
        }
        Op::PointerButton => {
            let Some(pressed) = request.get("pressed").and_then(Json::as_bool) else {
                return refusal("parse_error", "`pointer_button` takes `pressed`");
            };
            let button = match button(request, "button") {
                Ok(button) => button,
                Err(refusal) => return refusal,
            };
            seat_answer(compositor.seat_input(seat, &InputEvent::PointerButton { button, pressed }))
        }
        Op::Drag => seat_dragged(request, seat, compositor),
        Op::Scroll => {
            let dx = number(request, "dx").unwrap_or(0.0);
            let dy = number(request, "dy").unwrap_or(0.0);
            // A notch up is the old wire's positive `dy`; the compositor speaks the protocol's
            // axis, which counts down as positive.
            seat_answer(compositor.seat_input(
                seat,
                &InputEvent::PointerAxis {
                    horizontal: -dx,
                    vertical: -dy,
                },
            ))
        }
        Op::Key | Op::KeyDown | Op::KeyUp => seat_keys(op, request, seat, compositor),
        Op::Type => {
            let Some(text) = request.get("text").and_then(Json::as_str) else {
                return refusal("parse_error", "`type` takes `text`");
            };
            match compositor.seat_text(seat, text) {
                Ok(true) => json!({"ok": {}}),
                Ok(false) => refusal(
                    "input_failed",
                    "the screen is locked; a seat acts nothing there",
                ),
                Err(error) => refusal("input_failed", &error.to_string()),
            }
        }
        _ => refusal(
            "unsupported",
            &format!("`{}` answers nothing at a seat", op.name()),
        ),
    }
}

/// A seat's frame, written to the path the command names.
fn seat_snapshot(request: &Json, seat: tilo_compositor::Seat, compositor: &mut Compositor) -> Json {
    let path = request
        .get("path")
        .and_then(Json::as_str)
        .unwrap_or_default();
    match compositor.seat_screenshot(seat) {
        Ok(shot) => match seat_png(&shot, path) {
            Ok(()) => json!({"ok": {"frame": 0}}),
            Err((code, message)) => refusal(code, &message),
        },
        Err(message) => refusal("snapshot_failed", &message),
    }
}

/// The seat's pointer pressed at one point, dragged to another, and let go there.
fn seat_dragged(request: &Json, seat: tilo_compositor::Seat, compositor: &mut Compositor) -> Json {
    let (Some(x_from), Some(y_from), Some(x_to), Some(y_to)) = (
        number(request, "x_from"),
        number(request, "y_from"),
        number(request, "x_to"),
        number(request, "y_to"),
    ) else {
        return refusal(
            "parse_error",
            "`drag` takes `x_from`, `y_from`, `x_to` and `y_to`",
        );
    };
    let steps = request
        .get("steps")
        .and_then(Json::as_u64)
        .unwrap_or(8)
        .clamp(1, 1_000);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a drag's steps are clamped to a thousand"
    )]
    let steps = steps as u32;
    let button = match button(request, "button") {
        Ok(button) => button,
        Err(refused) => return refused,
    };
    let from = seat_point(compositor, seat, x_from, y_from);
    let to = seat_point(compositor, seat, x_to, y_to);
    match seat_drag(compositor, seat, button, from, to, steps) {
        Ok(()) => json!({"ok": {}}),
        Err(error) => refusal("input_failed", &error),
    }
}

/// The seat types the key `op` names: pressed and let go, pressed only, or let go only.
fn seat_keys(
    op: Op,
    request: &Json,
    seat: tilo_compositor::Seat,
    compositor: &mut Compositor,
) -> Json {
    let Some(code) = key_code(request, compositor) else {
        return refusal(
            "parse_error",
            "`key` takes a named key or one character the keyboard types",
        );
    };
    let mut heard = Ok(());
    for pressed in match op {
        Op::Key => &[true, false][..],
        Op::KeyDown => &[true][..],
        _ => &[false][..],
    } {
        let typed = compositor
            .seat_input(
                seat,
                &InputEvent::Key {
                    keycode: code,
                    pressed: *pressed,
                },
            )
            .map_err(|error| error.to_string());
        match typed {
            Ok(true) => {}
            Ok(false) => {
                return refusal(
                    "input_failed",
                    "the screen is locked; a seat acts nothing there",
                );
            }
            Err(error) => {
                heard = Err(error);
                break;
            }
        }
    }
    match heard {
        Ok(()) => json!({"ok": {}}),
        Err(error) => refusal("input_failed", &error),
    }
}

/// The seat `agent` drives, opened on its first command and aimed at `window` as that window
/// stands now, which a move since the last command changes. A seat the session ended — the
/// person's Stop, a lock — is opened again, aimed at the window the next command names.
fn seat_of(
    compositor: &mut Compositor,
    seats: &RefCell<BTreeMap<String, AgentSeat>>,
    agent: &str,
    window: Option<&str>,
) -> Result<tilo_compositor::Seat, String> {
    let mut opened = seats.borrow_mut();
    let held = opened.entry(agent.to_owned()).or_insert_with(|| AgentSeat {
        seat: compositor.open_seat(),
        window: None,
    });
    if compositor.seat_view(held.seat).is_none() {
        held.seat = compositor.open_seat();
        held.window = None;
    }
    let seat = held.seat;
    // A command that names a window, and one that takes a seat off the window it aimed at,
    // aim again: the place a window stands is read now, not when it was last named.
    let reaim = window.is_some() || held.window.is_some();
    drop(opened);
    if reaim {
        compositor.seat_aim_app(seat, window)?;
        if let Some(held) = seats.borrow_mut().get_mut(agent) {
            held.window = window.map(str::to_owned);
        }
    }
    Ok(seat)
}

/// A point the seat's command names, in the layout: a window's pixels are measured from its
/// own top-left.
fn seat_point(compositor: &Compositor, seat: tilo_compositor::Seat, x: f64, y: f64) -> (f64, f64) {
    match compositor.seat_view(seat).and_then(|aim| aim.window) {
        Some(window) => (window.rect.0 + x, window.rect.1 + y),
        None => (x, y),
    }
}

/// The answer to what a seat's event did: taken, or refused by a locked screen or a display
/// that could not take it.
fn seat_answer(result: Result<bool, tilo_compositor::Error>) -> Json {
    match result {
        Ok(true) => json!({"ok": {}}),
        Ok(false) => refusal(
            "input_failed",
            "the screen is locked; a seat acts nothing there",
        ),
        Err(error) => refusal("input_failed", &error.to_string()),
    }
}

/// A press at `from`, `steps` motions spread evenly to `to`, and the release there, as the seat
/// drags: its own button, in its own window.
fn seat_drag(
    compositor: &mut Compositor,
    seat: tilo_compositor::Seat,
    button: u32,
    from: (f64, f64),
    to: (f64, f64),
    steps: u32,
) -> Result<(), String> {
    let input = |compositor: &mut Compositor, event: InputEvent| {
        compositor
            .seat_input(seat, &event)
            .map(drop)
            .map_err(|error| error.to_string())
    };
    input(
        compositor,
        InputEvent::PointerMotion {
            x: from.0,
            y: from.1,
        },
    )?;
    input(
        compositor,
        InputEvent::PointerButton {
            button,
            pressed: true,
        },
    )?;
    for step in 1..=steps {
        let (x, y) = drag_step(from, to, f64::from(step) / f64::from(steps));
        input(compositor, InputEvent::PointerMotion { x, y })?;
    }
    input(
        compositor,
        InputEvent::PointerButton {
            button,
            pressed: false,
        },
    )
}

/// Write a seat's frame to `path` as a PNG.
fn seat_png(shot: &tilo_compositor::Screenshot, path: &str) -> Result<(), (&'static str, String)> {
    if path.is_empty() {
        return Err(("invalid_path", "snapshot path must not be empty".to_owned()));
    }
    let image = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba.clone()).ok_or_else(
        || {
            (
                "snapshot_failed",
                "the frame is not as large as it says".to_owned(),
            )
        },
    )?;
    image
        .save_with_format(path, image::ImageFormat::Png)
        .map_err(|error| ("snapshot_failed", format!("{path}: {error}")))
}
