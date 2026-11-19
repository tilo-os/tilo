//! A cache for storing the results of layout computation
use crate::geometry::Size;
use crate::style::AvailableSpace;
use crate::tree::{LayoutInput, LayoutOutput, RunMode};

/// The number of cache entries for each node in the tree
const CACHE_SIZE: usize = 9;

/// Cached intermediate layout results
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub(crate) struct CacheEntry<T> {
    /// The initial cached size of the node itself
    known_dimensions: Size<Option<f32>>,
    /// The initial cached size of the parent's node
    available_space: Size<AvailableSpace>,
    /// The cached size and baselines of the item
    content: T,
    /// Which of the sizes it came to its content alone does not give back at that size (see
    /// [`answers`])
    tainted: Taint,
}

/// Per axis, whether a node solved with that dimension open came to a size that laying it out
/// at that size does not reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Taint {
    /// A measurement or a minimum width stood the node wider than its content's wrapping.
    pub width: bool,
    /// A minimum height stood the node taller than its content, or a child's height resolves
    /// against the node's only once that is known.
    pub height: bool,
}

impl Taint {
    /// Both axes tainted: an entry that answers only the space it was measured under.
    pub const ALL: Self = Self { width: true, height: true };
}

/// A cache for caching the results of a sizing a Grid Item or Flexbox Item
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Cache {
    /// The cache entry for the node's final layout
    final_layout_entry: Option<CacheEntry<LayoutOutput>>,
    /// The cache entries for the node's preliminary size measurements
    measure_entries: [Option<CacheEntry<Size<f32>>>; CACHE_SIZE],
    /// Tracks if all cache entries are empty
    is_empty: bool,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether `entry`, solved to `cached_size`, answers a query with `known_dimensions` under
/// `available_space`.
///
/// An entry answers when it was solved with the dimensions the query knows. Where the query
/// knows a dimension the entry left open, the entry came to that size by itself, and it answers
/// when laying the node out at that size gives the same result: always when it was measured under
/// the space the query offers, and under other space only when that axis is not tainted. Under
/// a min-content constraint a block of text wraps into its narrowest lines, so a card holding it
/// comes to the height of that wrapping; a card whose content names a width it will not go under
/// (`min-width: 400px`) comes to that same width there, and its height at that width is another.
/// The solve that stored such an entry saw a measurement or a minimum width come to a width its
/// content's wrapping does not fill, and tainted it. An untainted entry is what the node lays out
/// to at its own size, and answering with it keeps a nested grid or flex container from solving
/// its subtree again for each constraint its parents probe it under.
fn answers<T: Copy>(
    entry: &CacheEntry<T>,
    known_dimensions: Size<Option<f32>>,
    available_space: Size<AvailableSpace>,
    cached_size: Size<f32>,
) -> bool {
    let axis_answers = |known: Option<f32>,
                        entry_known: Option<f32>,
                        cached: f32,
                        entry_available: AvailableSpace,
                        available: AvailableSpace,
                        relaxed: bool| {
        known == entry_known
            || (known == Some(cached) && (relaxed || entry_available.is_roughly_equal(available)))
    };

    axis_answers(
        known_dimensions.width,
        entry.known_dimensions.width,
        cached_size.width,
        entry.available_space.width,
        available_space.width,
        !entry.tainted.width,
    ) && axis_answers(
        known_dimensions.height,
        entry.known_dimensions.height,
        cached_size.height,
        entry.available_space.height,
        available_space.height,
        !entry.tainted.height,
    ) && (known_dimensions.width.is_some() || entry.available_space.width.is_roughly_equal(available_space.width))
        && (known_dimensions.height.is_some() || entry.available_space.height.is_roughly_equal(available_space.height))
}

impl Cache {
    /// Create a new empty cache
    pub const fn new() -> Self {
        Self { final_layout_entry: None, measure_entries: [None; CACHE_SIZE], is_empty: true }
    }

    /// Return the cache slot to cache the current computed result in
    ///
    /// ## Caching Strategy
    ///
    /// We need multiple cache slots, because a node's size is often queried by it's parent multiple times in the course of the layout
    /// process, and we don't want later results to clobber earlier ones.
    ///
    /// The two variables that we care about when determining cache slot are:
    ///
    ///   - How many "known_dimensions" are set. In the worst case, a node may be called first with neither dimension known, then with one
    ///     dimension known (either width of height - which doesn't matter for our purposes here), and then with both dimensions known.
    ///   - Whether unknown dimensions are being sized under a min-content or a max-content available space constraint (definite available space
    ///     shares a cache slot with max-content because a node will generally be sized under one or the other but not both).
    ///
    /// ## Cache slots:
    ///
    /// - Slot 0: Both known_dimensions were set
    /// - Slots 1-4: 1 of 2 known_dimensions were set and:
    ///   - Slot 1: width but not height known_dimension was set and the other dimension was either a MaxContent or Definite available space constraintraint
    ///   - Slot 2: width but not height known_dimension was set and the other dimension was a MinContent constraint
    ///   - Slot 3: height but not width known_dimension was set and the other dimension was either a MaxContent or Definite available space constraintable space constraint
    ///   - Slot 4: height but not width known_dimension was set and the other dimension was a MinContent constraint
    /// - Slots 5-8: Neither known_dimensions were set and:
    ///   - Slot 5: x-axis available space is MaxContent or Definite and y-axis available space is MaxContent or Definite
    ///   - Slot 6: x-axis available space is MaxContent or Definite and y-axis available space is MinContent
    ///   - Slot 7: x-axis available space is MinContent and y-axis available space is MaxContent or Definite
    ///   - Slot 8: x-axis available space is MinContent and y-axis available space is MinContent
    #[inline]
    fn compute_cache_slot(known_dimensions: Size<Option<f32>>, available_space: Size<AvailableSpace>) -> usize {
        use AvailableSpace::{Definite, MaxContent, MinContent};

        let has_known_width = known_dimensions.width.is_some();
        let has_known_height = known_dimensions.height.is_some();

        // Slot 0: Both known_dimensions were set
        if has_known_width && has_known_height {
            return 0;
        }

        // Slot 1: width but not height known_dimension was set and the other dimension was either a MaxContent or Definite available space constraint
        // Slot 2: width but not height known_dimension was set and the other dimension was a MinContent constraint
        if has_known_width && !has_known_height {
            return 1 + (available_space.height == MinContent) as usize;
        }

        // Slot 3: height but not width known_dimension was set and the other dimension was either a MaxContent or Definite available space constraint
        // Slot 4: height but not width known_dimension was set and the other dimension was a MinContent constraint
        if has_known_height && !has_known_width {
            return 3 + (available_space.width == MinContent) as usize;
        }

        // Slots 5-8: Neither known_dimensions were set and:
        match (available_space.width, available_space.height) {
            // Slot 5: x-axis available space is MaxContent or Definite and y-axis available space is MaxContent or Definite
            (MaxContent | Definite(_), MaxContent | Definite(_)) => 5,
            // Slot 6: x-axis available space is MaxContent or Definite and y-axis available space is MinContent
            (MaxContent | Definite(_), MinContent) => 6,
            // Slot 7: x-axis available space is MinContent and y-axis available space is MaxContent or Definite
            (MinContent, MaxContent | Definite(_)) => 7,
            // Slot 8: x-axis available space is MinContent and y-axis available space is MinContent
            (MinContent, MinContent) => 8,
        }
    }

    /// Try to retrieve a cached result from the cache
    #[inline]
    pub fn get(&self, input: &LayoutInput) -> Option<LayoutOutput> {
        self.get_tainted(input).map(|(output, _)| output)
    }

    /// A cached result for `input`, and whether it is tainted (see [`answers`]).
    #[inline]
    pub fn get_tainted(&self, input: &LayoutInput) -> Option<(LayoutOutput, Taint)> {
        let known_dimensions = input.known_dimensions;
        let available_space = input.available_space;

        match input.run_mode {
            RunMode::PerformLayout => self
                .final_layout_entry
                .filter(|entry| answers(entry, known_dimensions, available_space, entry.content.size))
                .map(|e| (e.content, e.tainted)),
            RunMode::ComputeSize => {
                for entry in self.measure_entries.iter().flatten() {
                    if answers(entry, known_dimensions, available_space, entry.content) {
                        return Some((LayoutOutput::from_outer_size(entry.content), entry.tainted));
                    }
                }

                None
            }
            RunMode::PerformHiddenLayout => None,
        }
    }

    /// Store a computed size in the cache
    pub fn store(&mut self, input: &LayoutInput, layout_output: LayoutOutput) {
        self.store_tainted(input, layout_output, Taint::ALL);
    }

    /// Store a computed size in the cache, tainted or not (see [`answers`]).
    pub fn store_tainted(&mut self, input: &LayoutInput, layout_output: LayoutOutput, tainted: Taint) {
        let known_dimensions = input.known_dimensions;
        let available_space = input.available_space;

        match input.run_mode {
            RunMode::PerformLayout => {
                self.is_empty = false;
                self.final_layout_entry =
                    Some(CacheEntry { known_dimensions, available_space, content: layout_output, tainted })
            }
            RunMode::ComputeSize => {
                self.is_empty = false;
                let cache_slot = Self::compute_cache_slot(known_dimensions, available_space);
                self.measure_entries[cache_slot] =
                    Some(CacheEntry { known_dimensions, available_space, content: layout_output.size, tainted });
            }
            RunMode::PerformHiddenLayout => {}
        }
    }

    /// Clear all cache entries and reports clear operation outcome ([`ClearState`])
    pub fn clear(&mut self) -> ClearState {
        if self.is_empty {
            return ClearState::AlreadyEmpty;
        }
        self.is_empty = true;
        self.final_layout_entry = None;
        self.measure_entries = [None; CACHE_SIZE];
        ClearState::Cleared
    }

    /// Returns true if all cache entries are None, else false
    pub fn is_empty(&self) -> bool {
        self.final_layout_entry.is_none() && !self.measure_entries.iter().any(|entry| entry.is_some())
    }
}

/// Clear operation outcome. See [`Cache::clear`]
pub enum ClearState {
    /// Cleared some values
    Cleared,
    /// Everything was already cleared
    AlreadyEmpty,
}
