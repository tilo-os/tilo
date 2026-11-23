//! Per-presence views of one open artwork: one per remote presence,
//! beside this screen's own. A presence's selection is published under its
//! own identity, so what one has selected never shapes another's view.
//!
//! Where a presence is, is said in the artwork's own terms: a range over
//! its marks, in the order every member holds them, so a peer's
//! selection resolves on the other machine without coordinates.
//!
//! - **Keyed by [`PresenceId`]**, which the host derives from the
//!   authenticated invoker: the view routes, it is never authority.
//! - **Bounded**: at most [`MAX_PEER_VIEWS`] views (least recently used
//!   evicted first), dropped after [`PEER_VIEW_TTL`] idle.
//! - **Never in the document**: presentation state the widget holds.

use std::collections::HashMap;
use std::ops::Range;

use ui::tilo_actions::PresenceId;
use ui::time::{Duration, Instant};

use crate::model::Artwork;

pub const MAX_PEER_VIEWS: usize = 64;
pub const PEER_VIEW_TTL: Duration = Duration::from_secs(10 * 60);
pub const MAX_PEER_LABEL_BYTES: usize = 128;
/// The item a presence's place in an artwork is named by: the artwork
/// itself. A [`Place`](ui::tilo_resource::Place) range over it counts marks.
pub use canvas_engine::resource::SELECTION_ITEM;

/// Where one presence is in the artwork: marks `start..end`, in the order
/// the artwork keeps them (bottom to top).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerView {
    pub marks: Range<usize>,
}

impl PeerView {
    /// The view of the marks at `start..end`, clamped to an artwork of
    /// `marks`.
    pub fn new(start: usize, end: usize, marks: usize) -> Self {
        let start = start.min(marks);
        let end = end.min(marks);
        Self {
            marks: start.min(end)..start.max(end),
        }
    }

    /// The marks this view selects, in an artwork of `art`.
    pub fn picked<'a>(&self, art: &'a Artwork) -> impl Iterator<Item = &'a str> + 'a {
        let marks = &self.marks;
        art.marks
            .iter()
            .skip(marks.start)
            .take(marks.end.saturating_sub(marks.start))
            .map(|mark| mark.id.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }
}

#[derive(Clone, Debug)]
struct Entry {
    view: PeerView,
    label: Option<String>,
    touched: Instant,
}

/// The presence views of one canvas.
#[derive(Debug, Default)]
pub struct PeerViews {
    entries: HashMap<PresenceId, Entry>,
}

impl PeerViews {
    pub fn contains(&self, presence: &PresenceId) -> bool {
        self.entries.contains_key(presence)
    }

    /// A presence joined with an empty selection; `true` when it is new.
    pub fn join(&mut self, presence: &PresenceId, now: Instant) -> bool {
        if self.entries.contains_key(presence) {
            return false;
        }
        self.evict(now);
        self.entries.insert(
            presence.clone(),
            Entry {
                view: PeerView::default(),
                label: None,
                touched: now,
            },
        );
        true
    }

    pub fn leave(&mut self, presence: &PresenceId) {
        self.entries.remove(presence);
    }

    pub fn set_label(&mut self, presence: &PresenceId, label: &str) {
        let label = truncate(label);
        if let Some(entry) = self.entries.get_mut(presence) {
            entry.label = Some(label);
        }
    }

    pub fn view(&self, presence: &PresenceId) -> Option<&PeerView> {
        self.entries.get(presence).map(|entry| &entry.view)
    }

    /// Store `presence`'s view. `false` when it holds no view here — an
    /// expired binding, or one that was never joined — in which case nothing
    /// is created: a place nobody's view asked for is not presence.
    pub fn set(&mut self, presence: &PresenceId, view: PeerView, now: Instant) -> bool {
        self.evict(now);
        let Some(entry) = self.entries.get_mut(presence) else {
            return false;
        };
        entry.view = view;
        entry.touched = now;
        true
    }

    /// Retire views nobody has touched for [`PEER_VIEW_TTL`], then the least
    /// recently used ones past [`MAX_PEER_VIEWS`].
    pub fn evict(&mut self, now: Instant) {
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.touched) < PEER_VIEW_TTL);
        while self.entries.len() >= MAX_PEER_VIEWS {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(presence, _)| presence.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PresenceId, &PeerView, Option<&str>)> {
        self.entries
            .iter()
            .map(|(presence, entry)| (presence, &entry.view, entry.label.as_deref()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// `label`, cut to [`MAX_PEER_LABEL_BYTES`] at a character boundary.
fn truncate(label: &str) -> String {
    let label = label.trim();
    if label.len() <= MAX_PEER_LABEL_BYTES {
        return label.to_owned();
    }
    let mut end = MAX_PEER_LABEL_BYTES;
    while end > 0 && !label.is_char_boundary(end) {
        end -= 1;
    }
    label.get(..end).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presence(id: &str) -> PresenceId {
        PresenceId::new(id).unwrap()
    }

    fn art() -> Artwork {
        let mut doc = ui::tilo_resource::Doc::new();
        <Artwork as ui::tilo_resource::Resource>::initialize(&mut doc).unwrap();
        for action in crate::sample::shapes() {
            Artwork::apply_to(&mut doc, action);
        }
        Artwork::from_doc(&doc).unwrap()
    }

    tilo_test_env::scenario! {
        /// The views presences hold of an artwork.
        /// Each case was a test of its own and builds its own fixture; a failure names its phase.
        fn a_peer_view_is_joined_selects_and_keeps_its_bounds;

        phase a_joined_view_starts_empty_and_is_announced_once;
        {
            let now = Instant::now();
            let mut views = PeerViews::default();
            let task = presence("agent-task:one");
            assert!(views.join(&task, now));
            assert!(!views.join(&task, now), "joining twice changes nothing");
            assert_eq!(views.view(&task), Some(&PeerView::default()));
            views.leave(&task);
            assert!(views.is_empty());
        }

        phase a_view_selects_the_marks_at_its_range;
        {
            let art = art();
            let expected: Vec<&str> = art
                .marks
                .iter()
                .skip(1)
                .take(2)
                .map(|mark| mark.id.as_str())
                .collect();
            let view = PeerView::new(1, 3, art.marks.len());
            assert_eq!(view.picked(&art).collect::<Vec<_>>(), expected);
            // A range past the artwork's marks selects only what is there.
            let over = PeerView::new(1, usize::MAX, art.marks.len());
            assert_eq!(over.picked(&art).count(), art.marks.len() - 1);
        }

        phase a_view_that_was_never_joined_is_not_presence;
        {
            let now = Instant::now();
            let mut views = PeerViews::default();
            let stray = presence("agent-task:stray");
            assert!(!views.set(&stray, PeerView::new(0, 1, 4), now));
            assert!(views.is_empty(), "nothing is created for it");
        }

        phase old_and_excess_views_are_evicted_so_a_stale_binding_finds_none;
        {
            let now = Instant::now();
            let mut views = PeerViews::default();
            let stale = presence("agent-task:stale");
            views.join(&stale, now.checked_sub(PEER_VIEW_TTL).unwrap());
            views.join(&presence("agent-task:fresh"), now);
            views.evict(now);
            assert!(!views.contains(&stale));
            assert!(
                !views.set(&stale, PeerView::new(0, 1, 4), now),
                "an expired binding cannot be written back"
            );

            for index in 0..MAX_PEER_VIEWS {
                views.join(&presence(&format!("agent-task:{index:03}")), now);
            }
            assert!(views.len() <= MAX_PEER_VIEWS, "{} views held", views.len());
            assert!(
                views.contains(&presence(&format!(
                    "agent-task:{:03}",
                    MAX_PEER_VIEWS - 1
                ))),
                "the views joined most recently survive"
            );
        }

        phase a_long_label_is_cut_to_its_bound;
        {
            let now = Instant::now();
            let mut views = PeerViews::default();
            let task = presence("agent-task:one");
            views.join(&task, now);
            views.set_label(&task, &"é".repeat(MAX_PEER_LABEL_BYTES));
            let (_, _, label) = views.iter().next().unwrap();
            assert!(label.unwrap().len() <= MAX_PEER_LABEL_BYTES);
        }
    }
}
