//! What every keyed row list does alike: [`super::VirtualList`],
//! [`super::TreeView`] and [`super::DataTable`] all move the focus by the
//! same keys, choose rows by the same gestures, count a double click the
//! same way and glide to a revealed row the same way. Each list keeps only
//! what its rows are (a flattened tree, a table's columns).

use winit::keyboard::{Key, NamedKey};

use crate::selection::KeySelection;
use crate::time::Instant;
use crate::virtual_viewport::{ScrollAlignment, ScrollMomentum, SparseVirtualViewport};

/// Where a navigation key moves a list's focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowStep {
    /// To this row.
    To(usize),
    /// A page on (`true`) or back from the focused row.
    Page(bool),
}

/// Where `key` moves the focus of a list of `len` rows focused on
/// `current`: the arrows by a row, Home and End to the ends, the page keys
/// by a page. `None` for any other key, or an empty list.
pub(crate) fn row_step(key: &Key, current: Option<usize>, len: usize) -> Option<RowStep> {
    let last = len.checked_sub(1)?;
    Some(match key {
        Key::Named(NamedKey::ArrowDown) => {
            RowStep::To(current.map_or(0, |index| index.saturating_add(1).min(last)))
        }
        Key::Named(NamedKey::ArrowUp) => {
            RowStep::To(current.map_or(0, |index| index.saturating_sub(1)))
        }
        Key::Named(NamedKey::Home) => RowStep::To(0),
        Key::Named(NamedKey::End) => RowStep::To(last),
        Key::Named(NamedKey::PageDown) => RowStep::Page(true),
        Key::Named(NamedKey::PageUp) => RowStep::Page(false),
        _ => return None,
    })
}

/// Whether `key`, with the command modifier held (`toggle`), selects every row.
pub(crate) fn is_select_all(key: &Key, toggle: bool) -> bool {
    toggle && matches!(key, Key::Character(text) if text.as_str().eq_ignore_ascii_case("a"))
}

/// A click, arrow or type-ahead choosing `key`: extend from the anchor to
/// it (`extend`, rows in order from `key_at` between the anchor's and its
/// index), toggle it (`toggle`), or choose it alone. A list that chooses one
/// row at a time (`multi` false) always chooses it alone.
pub(crate) fn choose<K>(
    selection: &mut KeySelection<K>,
    key: K,
    gesture: (bool, bool),
    multi: bool,
    index_of: impl Fn(&K) -> Option<usize>,
    key_at: impl FnMut(usize) -> Option<K>,
) where
    K: Clone + Eq + std::hash::Hash,
{
    let (toggle, extend) = gesture;
    if multi && extend {
        let between = selection
            .anchor()
            .and_then(|anchor| Some((index_of(anchor)?, index_of(&key)?)));
        let Some((anchor, target)) = between else {
            selection.select_only(key);
            return;
        };
        let keys = (anchor.min(target)..=anchor.max(target)).filter_map(key_at);
        selection.select_range(key, keys, toggle);
    } else if multi && toggle {
        selection.toggle(key);
    } else {
        selection.select_only(key);
    }
}

/// The last click a list took, to tell a double click.
pub(crate) struct Clicks<K> {
    last: Option<(K, Instant)>,
}

impl<K> Default for Clicks<K> {
    fn default() -> Self {
        Self { last: None }
    }
}

impl<K: PartialEq> Clicks<K> {
    /// Whether a click on `key` now completes a double click.
    pub(crate) fn is_double(&self, key: &K) -> bool {
        self.last.as_ref().is_some_and(|(last, at)| {
            last == key
                && crate::time::now().saturating_duration_since(*at)
                    < crate::gesture::multi_click_interval()
        })
    }

    /// Count a click on `key`: whether it completes a double click, after
    /// which counting starts over.
    pub(crate) fn register(&mut self, key: K) -> bool {
        let double = self.is_double(&key);
        if double {
            self.reset();
        } else {
            self.record(key);
        }
        double
    }

    /// Note a click on `key` that completes no double click itself.
    pub(crate) fn record(&mut self, key: K) {
        self.last = Some((key, crate::time::now()));
    }

    /// Forget the last click.
    pub(crate) fn reset(&mut self) {
        self.last = None;
    }
}

/// How a list's rows scroll beyond the viewport's geometry: the wheel's
/// momentum and the glide to a revealed row, with the fade a long jump
/// brings its rows in with.
pub(crate) struct RowScroll {
    pub(crate) momentum: ScrollMomentum,
    /// The motion toward a row a reveal was asked to show.
    pub(crate) to: crate::motion::ScrollTo,
    /// Device pixels per logical pixel where the list last handled input.
    pub(crate) device_scale: f64,
    /// The rows' opacity in the latest draw and in the frame on screen:
    /// below 1 while they fade in after a long reveal.
    pub(crate) fade_drawn: f32,
    pub(crate) fade_shown: f32,
}

impl Default for RowScroll {
    fn default() -> Self {
        Self {
            momentum: ScrollMomentum::new(),
            to: crate::motion::ScrollTo::new(),
            device_scale: 1.0,
            fade_drawn: 1.0,
            fade_shown: 1.0,
        }
    }
}

impl RowScroll {
    /// Start showing row `index` of `viewport`, `width` wide. A reveal the
    /// person's hand or key just asked for (a search hit opened, a row
    /// picked elsewhere) moves there ([`crate::motion::ScrollTo`]): a short
    /// jump glides, a long one fades in at the row; one nobody asked for
    /// lands at once. Returns the offset to land at, or `None` while gliding
    /// (follow it with [`Self::follow`]).
    pub(crate) fn reveal<K: Clone + Eq + std::hash::Hash>(
        &mut self,
        viewport: &mut SparseVirtualViewport<K>,
        index: usize,
        width: f64,
        motion: crate::tokens::EffectiveMotion,
        now: Instant,
    ) -> Option<f64> {
        let drawn = viewport.offset();
        // Where showing the row takes the list, from where it is going.
        if let Some(target) = self.to.target() {
            viewport.set_offset(target.y);
        }
        viewport.scroll_to_index(index, ScrollAlignment::Nearest);
        let target = viewport.offset();
        viewport.set_offset(drawn);
        self.momentum.stop();
        let jump = self.to.go(
            vello::kurbo::Vec2::new(0.0, drawn),
            vello::kurbo::Vec2::new(0.0, target),
            vello::kurbo::Vec2::new(width, viewport.viewport()),
            motion,
            now,
        );
        (jump != crate::motion::ScrollJump::Glide).then_some(target)
    }

    /// Where a reveal in progress is drawn at `now`, if one is.
    pub(crate) fn follow(&mut self, now: Instant) -> Option<f64> {
        let at = self.to.offset_in_device_pixels(now, self.device_scale)?;
        let _ = self.to.arrived(now);
        Some(at.y)
    }

    /// A reveal still moves, its rows still fade in, or the frame on screen
    /// shows them faded.
    pub(crate) fn in_progress(&self) -> bool {
        self.to.target().is_some() || self.to.is_moving(crate::motion::frame_now()) || self.fading()
    }

    /// The rows fade in, in the frame drawn or the one on screen.
    pub(crate) fn fading(&self) -> bool {
        self.fade_drawn < 1.0 || self.fade_shown < 1.0
    }
}
