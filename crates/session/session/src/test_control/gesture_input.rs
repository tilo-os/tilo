//! The seat's touchpad gestures, as the control door hands them in: what a touchpad would report
//! for a swipe or a pinch, and the fingers a held swipe leaves on the pad.

use serde_json::{Value as Json, json};
use tilo_compositor::{Compositor, GestureEvent, InputEvent, gestures};

use super::{Op, number, refusal};

/// The answer to what `op` asks of the seat's gesture tracker, when it asks of it.
/// A swipe whose fingers stay down leaves them in `fingers_down`, and the next gesture asked for
/// goes on from there: a swipe of as many fingers carries it on, anything else lifts them first.
pub(super) fn asked_of_gestures(
    op: Op,
    request: &Json,
    compositor: &mut Compositor,
    fingers_down: &std::cell::Cell<Option<u32>>,
) -> Option<Json> {
    let events = match op {
        Op::Swipe => swipe_events(request, fingers_down.get()),
        Op::Pinch => pinch_events(request, fingers_down.get()),
        _ => return None,
    };
    Some(match events {
        Err(answer) => answer,
        Ok((events, down)) => {
            fingers_down.set(down);
            inject(compositor, &events)
        }
    })
}

/// How many updates a gesture is split into unless the request says: the tools' own default.
const DEFAULT_GESTURE_STEPS: u32 = 10;

/// The most updates one gesture is split into: what a touchpad reports over a long swipe.
const MAX_GESTURE_STEPS: u32 = 288;

/// The seat's own gesture is the shell's: fewer fingers are the focused client's, which the control
/// door refuses rather than delivering where a person's hand could not reach.
fn client_gesture(fingers: u32) -> Json {
    refusal(
        "client_gesture",
        &format!(
            "a {fingers}-finger gesture belongs to the focused client; the shell takes {} or \
             more",
            gestures::SHELL_FINGER_THRESHOLD
        ),
    )
}

/// A swipe of `fingers` fingers travelling `(dx, dy)` over `steps` updates: a begin, the updates,
/// and the release at the end unless the request keeps the fingers down. With `down` fingers
/// already on the pad from a swipe that kept them there, as many carry that gesture on without a
/// new begin, and a different count lifts them first. Hands back the fingers left down.
fn swipe_events(
    request: &Json,
    down: Option<u32>,
) -> Result<(Vec<GestureEvent>, Option<u32>), Json> {
    let fingers = fingers_of(request)?;
    if !gestures::claims(fingers) {
        return Err(client_gesture(fingers));
    }
    let (dx, dy) = (
        number(request, "dx").unwrap_or(0.0),
        number(request, "dy").unwrap_or(0.0),
    );
    let steps = gesture_steps(request);
    let release = request
        .get("release")
        .and_then(Json::as_bool)
        .unwrap_or(true);
    let n = f64::from(steps);
    let mut events = match down {
        Some(held) if held == fingers => Vec::new(),
        Some(_) => vec![
            GestureEvent::SwipeEnd { cancelled: false },
            GestureEvent::SwipeBegin { fingers },
        ],
        None => vec![GestureEvent::SwipeBegin { fingers }],
    };
    events.extend((0..steps).map(|_| GestureEvent::SwipeUpdate {
        dx: dx / n,
        dy: dy / n,
    }));
    if release {
        events.push(GestureEvent::SwipeEnd { cancelled: false });
    }
    Ok((events, (!release).then_some(fingers)))
}

/// A pinch of `fingers` fingers to the absolute `scale` over `steps` updates, lifting the fingers
/// a swipe left `down` first.
fn pinch_events(
    request: &Json,
    down: Option<u32>,
) -> Result<(Vec<GestureEvent>, Option<u32>), Json> {
    let fingers = fingers_of(request)?;
    if !gestures::claims(fingers) {
        return Err(client_gesture(fingers));
    }
    let scale = number(request, "scale").unwrap_or(1.0);
    let steps = gesture_steps(request);
    let n = f64::from(steps);
    let mut events: Vec<GestureEvent> = down
        .map(|_| GestureEvent::SwipeEnd { cancelled: false })
        .into_iter()
        .collect();
    events.push(GestureEvent::PinchBegin { fingers });
    events.extend((1..=steps).map(|step| GestureEvent::PinchUpdate {
        dx: 0.0,
        dy: 0.0,
        scale: 1.0 + (scale - 1.0) * f64::from(step) / n,
        rotation: 0.0,
    }));
    events.push(GestureEvent::PinchEnd { cancelled: false });
    Ok((events, None))
}

/// A request's `fingers` , which a gesture must name.
fn fingers_of(request: &Json) -> Result<u32, Json> {
    request
        .get("fingers")
        .and_then(Json::as_u64)
        .and_then(|fingers| u32::try_from(fingers).ok())
        .ok_or_else(|| refusal("parse_error", "a gesture takes `fingers`"))
}

/// A request's `steps` , by the tools' default and within what a touchpad reports.
fn gesture_steps(request: &Json) -> u32 {
    request
        .get("steps")
        .and_then(Json::as_u64)
        .map_or(DEFAULT_GESTURE_STEPS, |steps| {
            u32::try_from(steps).unwrap_or(u32::MAX)
        })
        .clamp(1, MAX_GESTURE_STEPS)
}

/// Feed `events` to the seat, in order.
fn inject(compositor: &mut Compositor, events: &[GestureEvent]) -> Json {
    for event in events {
        if let Err(error) = compositor.input(InputEvent::Gesture(*event)) {
            return refusal("input_failed", &error.to_string());
        }
    }
    json!({"ok": {}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_keeps_its_own_gestures_and_the_shell_takes_three() {
        let refused = swipe_events(&json!({"fingers": 2, "dx": 100.0, "dy": 0.0}), None)
            .expect_err("two fingers are the client's");
        assert_eq!(refused["err"]["code"], "client_gesture");
        assert!(
            refused["err"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("3 or more"))
        );

        let (events, down) = swipe_events(
            &json!({"fingers": 3, "dx": 300.0, "dy": 0.0, "steps": 3}),
            None,
        )
        .expect("three fingers are the shell's");
        assert_eq!(down, None, "a released swipe leaves no fingers down");
        assert_eq!(events.len(), 5);
        assert!(matches!(
            events.first(),
            Some(GestureEvent::SwipeBegin { fingers: 3 })
        ));
        assert!(matches!(events.last(), Some(GestureEvent::SwipeEnd { .. })));

        let (held, down) = swipe_events(&json!({"fingers": 3, "dx": 10.0, "release": false}), None)
            .expect("a swipe whose fingers stay down");
        assert_eq!(
            held.len(),
            1 + usize::try_from(DEFAULT_GESTURE_STEPS).unwrap()
        );
        assert_eq!(down, Some(3), "the fingers stay on the pad");
        // The next swipe of as many fingers carries the same gesture on, and its release lifts them.
        let (on, down) = swipe_events(&json!({"fingers": 3, "dx": 10.0, "steps": 2}), down)
            .expect("the held swipe goes on");
        assert!(matches!(on.first(), Some(GestureEvent::SwipeUpdate { .. })));
        assert!(matches!(on.last(), Some(GestureEvent::SwipeEnd { .. })));
        assert_eq!((on.len(), down), (3, None));
        // Four fingers after three lift the three first.
        let (other, _) = swipe_events(&json!({"fingers": 4, "dy": 10.0, "steps": 1}), Some(3))
            .expect("a swipe of other fingers");
        assert!(matches!(
            other.get(..2),
            Some([
                GestureEvent::SwipeEnd { .. },
                GestureEvent::SwipeBegin { fingers: 4 }
            ])
        ));

        let (events, _) = pinch_events(&json!({"fingers": 3, "scale": 2.0, "steps": 2}), None)
            .expect("a three-finger pinch");
        assert_eq!(events.len(), 4);
        assert!(matches!(
            events.get(1),
            Some(GestureEvent::PinchUpdate { scale, .. }) if (*scale - 1.5).abs() < f64::EPSILON
        ));
        assert!(matches!(
            events.get(2),
            Some(GestureEvent::PinchUpdate { scale, .. }) if (*scale - 2.0).abs() < f64::EPSILON
        ));

        let (clamped, _) =
            pinch_events(&json!({"fingers": 3, "scale": 2.0, "steps": 10_000}), None)
                .expect("a pinch takes the most steps a touchpad reports");
        assert_eq!(
            clamped.len(),
            2 + usize::try_from(MAX_GESTURE_STEPS).unwrap()
        );
    }
}
