//! Which application produced a piece of content, and what to draw when an
//! action carries it into a different one.
//!
//! # The two halves, and why the first one is needed at all
//!
//! An action already says where it *landed*: the address names a resource,
//! and the resource belongs to an app the shell has a window for. What no
//! single call knows is where the thing it placed *came from*.
//!
//! The action model does record it, just not in one place. An argument
//! declared [`Collects::Content`](qos_actions::Collects) is a content hash,
//! and a content hash "exists only because some earlier operation produced
//! the content it names". The bytes reach the shell exactly once, through
//! [`CollabRequest::OfferBlob`](ui::collaboration::CollabRequest::OfferBlob)
//! — an app's blobs live in its own sandbox and the collaboration socket is
//! the only door out — and the offer arrives on a route that names the app
//! that sent it.
//!
//! So: remember who offered which hash, and the pair `(offering app,
//! addressed app)` is a fact the system already held in two halves. Nothing
//! here guesses from timing, and nothing here pairs two apps merely because
//! both changed.
//!
//! # Why this is a queue and not a function call
//!
//! Noting a flight must not cost the action anything — the film this exists
//! for already carries tens of seconds of agent latency and cannot afford a
//! frame more. So the door's side of this is a push onto a bounded queue and
//! nothing else: no layout, no lookup of where any window is, no drawing.
//! The compositor drains the queue on the next frame it was going to render
//! anyway, and resolves the geometry then, when it is the thing that knows
//! it.
//!
//! That ordering also makes the record honest. A flight is pushed *after*
//! the action returned success, so an operation that was refused — the word
//! processor declining to place an image whose bytes are not on this machine
//! — leaves no record and draws nothing.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use ui::permissions::UserId;
use ui::presence::ActionFlight;

use crate::shell::state::{GroupId, StripId};

/// A crossing the compositor has resolved to two windows and is drawing.
///
/// The endpoints are *groups*, not points. Which pixels a group occupies
/// changes every frame the strip pans — and revealing the destination is
/// often part of the same gesture — so the object is anchored to the windows
/// and its coordinates re-derived, rather than the reverse.
pub struct LiveFlight {
    pub flight: ActionFlight,
    /// What to call the participant, for the chip riding the object.
    pub name: String,
    pub from: (StripId, GroupId),
    pub to: (StripId, GroupId),
    /// The payload decoded, when this machine holds the bytes. Decoded once
    /// here rather than per frame: a photograph re-decoded sixty times a
    /// second would cost more than everything else this draws.
    pub content: Option<vello::peniko::ImageData>,
}

static LIVE: OnceLock<Mutex<Vec<LiveFlight>>> = OnceLock::new();

fn live() -> &'static Mutex<Vec<LiveFlight>> {
    LIVE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Turn everything the door recorded into crossings the compositor can draw,
/// resolving each end's application to the window group hosting it.
///
/// `locate` is the caller's answer to "which group is this application in",
/// which only the compositor can give. Returns the destination groups, so the
/// caller can bring them into view: a crossing that lands off the edge of the
/// screen is not one anybody can watch.
///
/// A flight whose *either* end cannot be located is dropped rather than
/// approximated. The cases that reach here are real — the source window was
/// closed between the action and the frame, the destination is on another
/// workspace, the app belongs to a peer and has no window on this machine —
/// and each of them means there is no honest picture to draw. Nothing is
/// better than a wrong one.
pub fn promote(
    mut locate: impl FnMut(&str) -> Option<(StripId, GroupId)>,
    content_for: impl Fn(&str) -> Option<Vec<u8>>,
) -> Vec<(StripId, GroupId)> {
    let pending = take_pending();
    if pending.is_empty() {
        return Vec::new();
    }
    let mut reveal = Vec::new();
    let Ok(mut l) = live().lock() else {
        return reveal;
    };
    for p in pending {
        let (Some(from), Some(to)) = (locate(&p.from_app), locate(&p.to_app)) else {
            continue;
        };
        let content = content_for(&p.hash).and_then(|b| ui::image_store::decode_image(&b));
        reveal.push(to);
        l.push(LiveFlight {
            flight: ActionFlight::new(UserId(p.actor.clone()), p.hash),
            name: p.actor,
            from,
            to,
            content,
        });
    }
    reveal
}

/// Drop the crossings that have finished.
pub fn retire(now: Instant) {
    if let Ok(mut l) = live().lock() {
        l.retain(|f| f.flight.progress(now).is_some());
    }
}

/// Whether anything is crossing, so the caller keeps asking for frames.
pub fn any_live() -> bool {
    live().lock().map(|l| !l.is_empty()).unwrap_or(false)
}

/// Read the crossings in flight. Borrowed rather than cloned — a decoded
/// photograph per frame per flight is not worth copying.
pub fn with_live<R>(f: impl FnOnce(&[LiveFlight]) -> R) -> R {
    match live().lock() {
        Ok(l) => f(&l),
        Err(_) => f(&[]),
    }
}

/// Hold every crossing at a fixed point along its curve.
///
/// One offscreen frame costs far more wall time than the crossing lasts, so
/// stepping frames only ever catches an object that has not left and one that
/// has already arrived — the two moments that prove least. This backdates the
/// start so the production draw renders the curve at a chosen millisecond,
/// which is the same escape the strip's make-space slide and the wheel's
/// settle already offer, for the same reason.
pub fn hold_at(elapsed: std::time::Duration) -> bool {
    let Ok(mut l) = live().lock() else {
        return false;
    };
    if l.is_empty() {
        return false;
    }
    let now = Instant::now();
    for f in l.iter_mut() {
        f.flight.started = now.checked_sub(elapsed).unwrap_or(now);
    }
    true
}

/// How many recent offers to remember.
///
/// An offer ledger that grew without bound would hold every hash the session
/// ever saw. Bounded to the recent past instead, which is all this can
/// honestly speak for anyway: the question being answered is "did an app on
/// this machine just produce this?", and an offer from an hour ago is not
/// evidence that the app which sent it is still on screen.
const REMEMBERED_OFFERS: usize = 64;

/// How many un-drained flights to hold.
///
/// Small: the compositor drains this every frame, so anything beyond a
/// handful means frames are not happening, and in that case the useful
/// behaviour is to drop the backlog rather than to replay a burst of stale
/// motion once rendering resumes.
const PENDING_CAP: usize = 8;

/// One application handing content to another, as the door saw it.
///
/// Deliberately carries no geometry. Where the two applications are on
/// screen is the compositor's to answer, and answering it here would both
/// slow the action down and be wrong by the time it was drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingFlight {
    /// The participant that performed the action — `"claude"` for the agent.
    /// Becomes the colour, so the crossing reads as somebody having done it.
    pub actor: String,
    /// The content hash that travelled.
    pub hash: String,
    /// The app that offered the content.
    pub from_app: String,
    /// The app whose action was addressed.
    pub to_app: String,
}

#[derive(Default)]
struct Ledger {
    /// `(hash, app_id)`, most recent last.
    offers: VecDeque<(String, String)>,
    pending: VecDeque<PendingFlight>,
}

fn ledger() -> &'static Mutex<Ledger> {
    static LEDGER: OnceLock<Mutex<Ledger>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(Ledger::default()))
}

/// Remember that `app_id` offered the content behind `hash`.
///
/// Called where the bytes cross out of an app's sandbox. Re-offering the
/// same hash moves it to the front rather than duplicating it: content is
/// addressed by its bytes, so the same photograph offered twice is one
/// photograph, and the most recent offer is the one whose window is most
/// likely still open.
pub fn note_offer(hash: &str, app_id: &str) {
    if hash.is_empty() || app_id.is_empty() {
        return;
    }
    let Ok(mut l) = ledger().lock() else { return };
    l.offers.retain(|(h, _)| h != hash);
    l.offers.push_back((hash.to_string(), app_id.to_string()));
    while l.offers.len() > REMEMBERED_OFFERS {
        l.offers.pop_front();
    }
}

/// The app that offered `hash`, if one on this machine did.
///
/// `None` is an ordinary answer and means "no flight": content that arrived
/// from a peer, or was produced before this session, has no source window
/// here to leave from, and inventing one would be a picture of something
/// that did not happen.
pub fn source_app_for(hash: &str) -> Option<String> {
    let l = ledger().lock().ok()?;
    l.offers
        .iter()
        .rev()
        .find(|(h, _)| h == hash)
        .map(|(_, app)| app.clone())
}

/// Record that `actor` carried `hash` into `to_app`, if the content came
/// from a different application on this machine.
///
/// Returns whether anything was recorded, which is what the callers' tests
/// assert on. The three cases that record nothing are all deliberate:
/// unknown source (no proven dependency), same app (nothing crossed an
/// application boundary), empty hash (not a content argument at all).
pub fn note_flight(actor: &str, hash: &str, to_app: &str) -> bool {
    if actor.is_empty() || hash.is_empty() || to_app.is_empty() {
        return false;
    }
    let Some(from_app) = source_app_for(hash) else {
        return false;
    };
    // An app placing its own content into its own document has not moved
    // anything between applications, and drawing a crossing for it would
    // claim something the action does not support.
    if from_app == to_app {
        return false;
    }
    let Ok(mut l) = ledger().lock() else {
        return false;
    };
    l.pending.push_back(PendingFlight {
        actor: actor.to_string(),
        hash: hash.to_string(),
        from_app,
        to_app: to_app.to_string(),
    });
    while l.pending.len() > PENDING_CAP {
        l.pending.pop_front();
    }
    true
}

/// Take everything recorded since the last drain. The compositor's side.
pub fn take_pending() -> Vec<PendingFlight> {
    let Ok(mut l) = ledger().lock() else {
        return Vec::new();
    };
    l.pending.drain(..).collect()
}

/// Forget everything. For tests, which share one process and would
/// otherwise inherit each other's ledger.
#[cfg(test)]
pub fn reset() {
    if let Ok(mut l) = ledger().lock() {
        l.offers.clear();
        l.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn a_crossing_is_recorded_when_the_content_came_from_another_app() {
        reset();
        note_offer("abc123", "dev.qos.photos");
        assert!(note_flight("claude", "abc123", "dev.qos.word-processor"));
        let pending = take_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].from_app, "dev.qos.photos");
        assert_eq!(pending[0].to_app, "dev.qos.word-processor");
        assert_eq!(pending[0].actor, "claude");
        assert!(take_pending().is_empty(), "a drain must not repeat itself");
    }

    #[test]
    #[serial]
    fn content_nobody_here_produced_crosses_nothing() {
        reset();
        // The hash is well-formed and the action may well have succeeded —
        // but the bytes came from a peer, so there is no window on this
        // screen for the object to leave from.
        assert!(!note_flight("claude", "from-a-peer", "dev.qos.word-processor"));
        assert!(take_pending().is_empty());
    }

    #[test]
    #[serial]
    fn an_app_placing_its_own_content_is_not_a_crossing() {
        reset();
        note_offer("abc123", "dev.qos.word-processor");
        assert!(
            !note_flight("claude", "abc123", "dev.qos.word-processor"),
            "nothing crossed an application boundary here",
        );
    }

    #[test]
    #[serial]
    fn the_most_recent_offer_of_a_hash_wins_and_does_not_duplicate() {
        reset();
        note_offer("abc123", "dev.qos.photos");
        note_offer("abc123", "dev.qos.file-explorer");
        assert_eq!(source_app_for("abc123").as_deref(), Some("dev.qos.file-explorer"));
        // Re-offering must not consume two slots, or a renderer asking for
        // one photograph repeatedly would evict everything else.
        note_offer("other", "dev.qos.photos");
        for _ in 0..REMEMBERED_OFFERS {
            note_offer("abc123", "dev.qos.photos");
        }
        assert_eq!(
            source_app_for("other").as_deref(),
            Some("dev.qos.photos"),
            "one repeatedly-offered hash must not evict the rest of the ledger",
        );
    }

    #[test]
    #[serial]
    fn the_offer_ledger_forgets_the_distant_past_rather_than_growing() {
        reset();
        note_offer("oldest", "dev.qos.photos");
        for i in 0..REMEMBERED_OFFERS {
            note_offer(&format!("h{i}"), "dev.qos.photos");
        }
        assert!(source_app_for("oldest").is_none(), "must be evicted");
    }

    #[test]
    #[serial]
    fn a_backlog_of_undrawn_crossings_is_bounded() {
        reset();
        note_offer("abc123", "dev.qos.photos");
        for _ in 0..(PENDING_CAP * 3) {
            note_flight("claude", "abc123", "dev.qos.word-processor");
        }
        assert_eq!(
            take_pending().len(),
            PENDING_CAP,
            "frames stopped happening; replaying every missed one is worse than dropping them",
        );
    }
}
