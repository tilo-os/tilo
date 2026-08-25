//! Which resource an action was performed on, and what to draw on it.
//!
//! The sibling of [`crate::shell::action_flight`], for the case that is far
//! commoner and was entirely invisible: an operation on one resource, inside
//! one window. A crossing needs two applications and a content hash to prove a
//! dependency between them. A touch needs neither — the address the caller
//! invoked *is* the evidence, because it names the resource.
//!
//! # What the door already knew, and never said
//!
//! An invocation arrives as `<resource id>#<action id>`. Both halves are the
//! app's own ids, and the shell resolves them, checks the capability, and
//! dispatches. So at the moment of the act the shell holds: who asked, what
//! they addressed, and what the app calls that operation. Every part of the
//! sentence "Claude cropped the harbour photograph" is already in hand, and
//! until now none of it reached the screen — the document changed, the window
//! repainted, and a viewer was left to take somebody's word for who did it.
//!
//! What was genuinely missing is the *last* step, and it is not knowledge the
//! door can have: where on screen that resource is. A resource id carries no
//! position and must not, so the answer comes from the view, per frame, through
//! [`Widget::resource_rects`](ui::widget::Widget::resource_rects). This module
//! holds the half the door can answer and leaves the geometry to the compositor
//! — the same division, and for the same reasons, as the crossing next door.
//!
//! # Why this is a queue and not a function call
//!
//! Identical to [`crate::shell::action_flight`]: noting a touch must cost the
//! action nothing. The door's side is a push onto a bounded queue — no layout,
//! no window lookup, no drawing. The compositor drains it on a frame it was
//! going to render anyway and resolves the geometry then, when it is the thing
//! that knows it.
//!
//! # What is recorded, and what is not
//!
//! Only invocations that went through the door. An address that resolved to
//! nothing, an operation the capability gate refused, an argument that would
//! not bind — none of them reach here, so none of them mark anything. That is
//! the property that makes the mark evidence rather than decoration: it cannot
//! be shown for work that did not happen.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use ui::permissions::UserId;
use ui::presence::ActionTouch;

/// Which resource addresses each connected application is currently
/// publishing, by the app id it announced.
///
/// The out-of-process half of resolving an address to a place on screen, and
/// a deliberately coarser answer than the in-process one.
///
/// An app compiled into the compositor answers *exactly*, because it is a
/// widget: [`Widget::resource_rects`](ui::widget::Widget::resource_rects) hands
/// back the photograph's own rect, letterbox bands excluded. A packaged app is
/// a separate process, and the socket it holds carries what it can do
/// (`publishActions`) and no way at all to say where it drew any of it. So the
/// finest true statement available about a packaged app's resource is **the
/// window that published it** — which is what this index gives, and what the
/// mark is drawn on.
///
/// It is a real answer and not a stand-in: the window is where that object is
/// being shown, and a viewer looking at a marked window is looking at the thing
/// that changed. It is simply not the tighter answer, and it will not become
/// one until an application can tell the session where inside its window an
/// address is drawn — which needs a field on the wire and not a change here.
///
/// Refreshed from the published surface on the frames a mark needs it, rather
/// than kept in step continuously: the survey copies every connected app's
/// descriptors, and nothing is looking at this for the vast majority of frames.
static PUBLISHED: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();

fn published() -> &'static Mutex<HashMap<String, Vec<String>>> {
    PUBLISHED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Replace the whole index. Level-triggered, like the publish it is built
/// from: an app that has closed a document stops answering for it rather than
/// leaving an address pointing at a window that is no longer showing it.
pub fn set_published_resources(by_app: HashMap<String, Vec<String>>) {
    if let Ok(mut p) = published().lock() {
        *p = by_app;
    }
}

/// The addresses `app_id` is publishing, for a window that wants to say it is
/// the one showing them.
pub fn resources_of(app_id: &str) -> Vec<String> {
    published()
        .lock()
        .map(|p| p.get(app_id).cloned().unwrap_or_default())
        .unwrap_or_default()
}

/// How many un-drawn touches are kept.
///
/// Small, and it drops the oldest. A session whose frames stopped — minimised,
/// on another workspace, a compositor stalled behind a slow client — must come
/// back showing what is happening *now*, not replay a backlog of marks for
/// operations that finished while nobody was looking.
const PENDING_CAP: usize = 8;

/// One invocation, as the door saw it.
///
/// Carries no geometry, for the reason set out on
/// [`ui::presence::ActionTouch`]: where a resource is on screen changes every
/// frame, and often changes *because of the action itself*.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTouch {
    /// The participant that performed it.
    pub actor: String,
    /// The resource half of the address — what was acted on.
    pub resource: String,
    /// The action's display label as the app published it.
    pub action_label: String,
}

/// A touch the compositor has accepted and is drawing.
///
/// Unlike a crossing there is nothing to resolve up front: the address stays an
/// address and is looked up against the frame's resource rects each time. What
/// is resolved here is only what the *room* knows and the door does not — what
/// to call this participant, and whether they are an agent.
pub struct LiveTouch {
    pub touch: ActionTouch,
    /// What to call the participant, for the chip.
    pub name: String,
}

static LIVE: OnceLock<Mutex<Vec<LiveTouch>>> = OnceLock::new();

fn live() -> &'static Mutex<Vec<LiveTouch>> {
    LIVE.get_or_init(|| Mutex::new(Vec::new()))
}

fn pending() -> &'static Mutex<VecDeque<PendingTouch>> {
    static P: OnceLock<Mutex<VecDeque<PendingTouch>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Record that `actor` performed `action_label` on `resource`.
///
/// Returns whether anything was recorded. The empty cases record nothing: an
/// invocation with no actor cannot be attributed to anybody, and one with no
/// resource has nothing to point at.
///
/// Called from the door's own success path and nowhere else. A push and a
/// bound, and that is all — see the module docs on why this must not do work.
pub fn note_touch(actor: &str, resource: &str, action_label: &str) -> bool {
    if actor.is_empty() || resource.is_empty() {
        return false;
    }
    let Ok(mut p) = pending().lock() else {
        return false;
    };
    p.push_back(PendingTouch {
        actor: actor.to_string(),
        resource: resource.to_string(),
        action_label: action_label.to_string(),
    });
    while p.len() > PENDING_CAP {
        p.pop_front();
    }
    true
}

/// Turn everything the door recorded into marks the compositor can draw.
///
/// `describe` is the caller's answer to "what is this participant called, and
/// is it an agent" — the room's roster, which only the compositor can reach.
/// A participant the roster has no entry for keeps their raw id as a name and
/// is drawn as a person: an unknown participant is still a participant, and
/// guessing that one is a bot is a worse error than not saying.
pub fn promote(mut describe: impl FnMut(&str) -> (String, bool)) -> usize {
    let taken = take_pending();
    if taken.is_empty() {
        return 0;
    }
    let Ok(mut l) = live().lock() else {
        return 0;
    };
    let mut n = 0;
    for p in taken {
        let (name, is_agent) = describe(&p.actor);
        l.push(LiveTouch {
            touch: ActionTouch::new(
                UserId(p.actor),
                p.resource,
                p.action_label,
                is_agent,
            ),
            name,
        });
        n += 1;
    }
    n
}

/// Drop the marks that have finished.
pub fn retire(now: Instant) {
    if let Ok(mut l) = live().lock() {
        l.retain(|t| t.touch.progress(now).is_some());
    }
}

/// Whether anything is being touched, so the caller keeps asking for frames.
pub fn any_live() -> bool {
    live().lock().map(|l| !l.is_empty()).unwrap_or(false)
}

/// Read the live marks under the lock.
pub fn with_live<R>(f: impl FnOnce(&[LiveTouch]) -> R) -> R {
    match live().lock() {
        Ok(l) => f(&l),
        Err(_) => f(&[]),
    }
}

/// Hold every live mark at `elapsed` into its span.
///
/// The screenshot harness's way in. A capture takes far longer than the mark
/// lasts, so stepping frames only ever catches the ends of the curve; this
/// rewinds each mark's start so the next paint renders the production draw at a
/// chosen instant rather than a reconstruction of it. Returns whether there was
/// anything to hold.
pub fn hold_at(elapsed: std::time::Duration) -> bool {
    let Ok(mut l) = live().lock() else {
        return false;
    };
    if l.is_empty() {
        return false;
    }
    let now = Instant::now();
    for t in l.iter_mut() {
        t.touch.started = now - elapsed;
    }
    true
}

/// How many touches are waiting to be promoted.
pub fn pending_len() -> usize {
    pending().lock().map(|p| p.len()).unwrap_or(0)
}

/// Take everything recorded since the last call.
pub fn take_pending() -> Vec<PendingTouch> {
    match pending().lock() {
        Ok(mut p) => p.drain(..).collect(),
        Err(_) => Vec::new(),
    }
}

/// Forget everything, live and pending. Test-only: the stores are process-wide.
pub fn reset() {
    if let Ok(mut p) = pending().lock() {
        p.clear();
    }
    if let Ok(mut l) = live().lock() {
        l.clear();
    }
    if let Ok(mut p) = published().lock() {
        p.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use ui::presence::TOUCH_DURATION;

    /// The stores are process-wide statics, so the tests that write to them
    /// take one lock rather than racing each other.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        static G: OnceLock<Mutex<()>> = OnceLock::new();
        let g = G.get_or_init(|| Mutex::new(()));
        let held = g.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        held
    }

    #[test]
    fn an_invocation_is_recorded_with_who_and_what() {
        let _g = guard();
        assert!(note_touch("claude", "photos.image:/a.png", "Crop"));
        let p = take_pending();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].actor, "claude");
        assert_eq!(p[0].resource, "photos.image:/a.png");
        assert_eq!(p[0].action_label, "Crop");
    }

    /// Nothing to attribute it to, or nothing to point at — either way there is
    /// no honest mark to draw.
    #[test]
    fn an_unattributable_or_unaddressed_touch_records_nothing() {
        let _g = guard();
        assert!(!note_touch("", "photos.image:/a.png", "Crop"));
        assert!(!note_touch("claude", "", "Crop"));
        assert_eq!(pending_len(), 0);
    }

    /// A session that stopped painting comes back showing what is happening
    /// now, not a burst of stale marks.
    #[test]
    fn the_backlog_is_bounded_and_drops_the_oldest() {
        let _g = guard();
        note_touch("claude", "oldest", "Crop");
        for i in 0..PENDING_CAP {
            note_touch("claude", &format!("r{i}"), "Crop");
        }
        let p = take_pending();
        assert_eq!(p.len(), PENDING_CAP);
        assert!(
            !p.iter().any(|t| t.resource == "oldest"),
            "the oldest waiting mark is the one dropped"
        );
    }

    /// The roster supplies the name and the agent flag; the door supplies
    /// neither, and must not have to.
    #[test]
    fn promotion_takes_the_name_and_the_kind_from_the_room() {
        let _g = guard();
        note_touch("u-7", "photos.image:/a.png", "Rotate right");
        assert_eq!(promote(|id| (format!("Agent {id}"), true)), 1);
        with_live(|live| {
            assert_eq!(live.len(), 1);
            assert_eq!(live[0].name, "Agent u-7");
            assert!(live[0].touch.is_agent);
            assert_eq!(live[0].touch.actor.0, "u-7");
        });
    }

    /// A participant the room cannot describe is still drawn, as a person.
    #[test]
    fn an_unknown_participant_is_drawn_as_a_person_under_their_own_id() {
        let _g = guard();
        note_touch("stranger", "photos.image:/a.png", "Crop");
        promote(|id| (id.to_string(), false));
        with_live(|live| {
            assert_eq!(live[0].name, "stranger");
            assert!(!live[0].touch.is_agent);
        });
    }

    #[test]
    fn a_mark_retires_when_its_span_is_over() {
        let _g = guard();
        note_touch("claude", "photos.image:/a.png", "Crop");
        promote(|id| (id.to_string(), true));
        assert!(any_live());
        retire(Instant::now());
        assert!(any_live(), "still inside its span");
        retire(Instant::now() + TOUCH_DURATION + Duration::from_millis(1));
        assert!(!any_live());
    }

    /// A still capture has to be able to sit in the held middle of the curve,
    /// or no single frame ever shows what the mark looks like.
    #[test]
    fn a_capture_can_hold_a_mark_mid_span() {
        let _g = guard();
        assert!(!hold_at(TOUCH_DURATION / 2), "nothing live to hold");
        note_touch("claude", "photos.image:/a.png", "Crop");
        promote(|id| (id.to_string(), true));
        assert!(hold_at(TOUCH_DURATION / 2));
        with_live(|live| {
            let t = live[0].touch.progress(Instant::now()).expect("still live");
            assert!(
                (t - 0.5).abs() < 0.05,
                "held near the middle of the span, got {t}"
            );
            assert_eq!(ui::presence::touch_pose(t).offset, 0.0);
        });
    }
}
