//! Collision-safe text identities and bounded second-chance storage. Fingerprints
//! select a bucket only; retained ownership or exact content proves equality.

use super::{FontChoice, Hyphenation, OpticalSizing, TextStyle};
use std::cell::OnceCell;
use std::collections::{HashMap, VecDeque, hash_map};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Immutable text and logical style for repeated measurement and drawing.
/// Retain this value (or a cheap clone) while content/style is unchanged, then
/// construct a new one when either changes. Its hash is computed at construction;
/// a warm renderer lookup of that retained handle does not scan its payload.
/// Share clones of one handle between layout and paint; independently constructed
/// equal handles can require exact comparison when replacing each other's cache
/// identity. Promotion from a borrowed entry also respects the payload budget.
/// Width, renderer text scale, font generation and custom hyphenator versions are
/// still checked on every call. Colour and transform remain paint-time inputs.
#[derive(Clone)]
pub struct PreparedText(Arc<TextIdentity>);

impl PreparedText {
    pub fn new(text: impl Into<String>, style: TextStyle) -> Self {
        let text = text.into().into_boxed_str();
        let fingerprint = fingerprint(&text, &style);
        Self(Arc::new(TextIdentity {
            text,
            style,
            fingerprint,
        }))
    }

    pub fn text(&self) -> &str {
        &self.0.text
    }

    pub fn style(&self) -> &TextStyle {
        &self.0.style
    }
}

struct TextIdentity {
    text: Box<str>,
    style: TextStyle,
    fingerprint: u64,
}

impl TextIdentity {
    fn bytes(&self) -> usize {
        let style = &self.style;
        let family = match &style.family {
            FontChoice::Named(name) => name.capacity(),
            _ => 0,
        };
        let language = match &style.hyphenation {
            Hyphenation::Auto(tag) => tag.capacity(),
            _ => 0,
        };
        std::mem::size_of::<Self>()
            .saturating_add(2 * std::mem::size_of::<usize>()) // Arc counters
            .saturating_add(self.text.len())
            .saturating_add(family)
            .saturating_add(language)
            .saturating_add(
                style
                    .font_features
                    .capacity()
                    .saturating_mul(std::mem::size_of::<super::OpenTypeFeature>()),
            )
            .saturating_add(
                style
                    .font_variations
                    .capacity()
                    .saturating_mul(std::mem::size_of::<super::FontVariation>()),
            )
    }
}

fn fingerprint(text: &str, style: &TextStyle) -> u64 {
    #[cfg(test)]
    HASHED_BYTES.with(|count| count.set(count.get() + text.len()));
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hash);
    style.size.to_bits().hash(&mut hash);
    style.scaling.hash(&mut hash);
    style.line_height.to_bits().hash(&mut hash);
    style.weight.hash(&mut hash);
    style.italic.hash(&mut hash);
    style.family.cache_hash().hash(&mut hash);
    style.advanced_hash().hash(&mut hash);
    // The live version is captured separately, including for PreparedText.
    match &style.hyphenation {
        Hyphenation::None | Hyphenation::Auto(_) => style.hyphenation.cache_hash().hash(&mut hash),
        Hyphenation::Custom(_) => 2_u8.hash(&mut hash),
    }
    hash.finish()
}

fn same_style(a: &TextStyle, b: &TextStyle) -> bool {
    let hyphenation = match (&a.hyphenation, &b.hyphenation) {
        (Hyphenation::None, Hyphenation::None) => true,
        (Hyphenation::Auto(a), Hyphenation::Auto(b)) => a == b,
        // Different implementations may choose the same numeric cache_key.
        // Owning both Arcs prevents allocator address reuse from aliasing them.
        (Hyphenation::Custom(a), Hyphenation::Custom(b)) => Arc::ptr_eq(a, b),
        _ => false,
    };
    let optical = match (a.optical_sizing, b.optical_sizing) {
        (OpticalSizing::Auto, OpticalSizing::Auto) | (OpticalSizing::None, OpticalSizing::None) => {
            true
        }
        (OpticalSizing::Value(a), OpticalSizing::Value(b)) => a.to_bits() == b.to_bits(),
        _ => false,
    };
    a.size.to_bits() == b.size.to_bits()
        && a.line_height.to_bits() == b.line_height.to_bits()
        && a.weight == b.weight
        && a.italic == b.italic
        && a.family == b.family
        && a.letter_spacing.to_bits() == b.letter_spacing.to_bits()
        && a.font_features == b.font_features
        && a.font_variations.len() == b.font_variations.len()
        && a.font_variations
            .iter()
            .zip(&b.font_variations)
            .all(|(a, b)| a.tag == b.tag && a.value.to_bits() == b.value.to_bits())
        && optical
        && hyphenation
        && a.align == b.align
        && a.wrap_mode == b.wrap_mode
        && a.hanging_punctuation == b.hanging_punctuation
        && a.fragmentation == b.fragmentation
        && a.scaling == b.scaling
        && a.kind == b.kind
        && a.ground == b.ground
        && a.distinguish == b.distinguish
}

/// One public operation hashes borrowed input once. On its first exact match it
/// adopts the resident Arc, so subsequent width/measure/shape probes use identity.
/// No borrowed pointer is ever retained after the operation.
pub(super) struct CacheQuery<'a> {
    pub text: &'a str,
    pub style: &'a TextStyle,
    fingerprint: u64,
    retained: Option<&'a Arc<TextIdentity>>,
    resolved: OnceCell<Arc<TextIdentity>>,
    hyphenator_version: Option<u64>,
    // One rejected shape may be needed again by the same width/measure/draw
    // operation. It dies with this query, never with the long-lived renderer.
    uncached_shape: OnceCell<(BucketKey, super::ShapedText)>,
}

impl<'a> CacheQuery<'a> {
    pub fn new(text: &'a str, style: &'a TextStyle) -> Self {
        Self::with_identity(text, style, fingerprint(text, style), None)
    }

    pub fn prepared(input: &'a PreparedText) -> Self {
        Self::with_identity(
            input.text(),
            input.style(),
            input.0.fingerprint,
            Some(&input.0),
        )
    }

    fn with_identity(
        text: &'a str,
        style: &'a TextStyle,
        fingerprint: u64,
        retained: Option<&'a Arc<TextIdentity>>,
    ) -> Self {
        let hyphenator_version = match &style.hyphenation {
            Hyphenation::Custom(policy) => Some(policy.cache_key()),
            _ => None,
        };
        Self {
            text,
            style,
            fingerprint,
            retained,
            resolved: OnceCell::new(),
            hyphenator_version,
            uncached_shape: OnceCell::new(),
        }
    }

    pub fn key(&self, width: Option<f32>, generation: u64, scale: f32) -> CacheKey<'_> {
        CacheKey {
            input: self,
            bucket: BucketKey {
                fingerprint: self.fingerprint,
                width: width.map(f32::to_bits),
                generation,
                scale: scale.to_bits(),
                distinguish_everywhere: crate::font_catalog::matched::distinguish_everywhere(),
                hyphenator_version: self.hyphenator_version,
            },
        }
    }

    fn matches(&self, identity: &Arc<TextIdentity>) -> bool {
        if self
            .resolved
            .get()
            .is_some_and(|ours| Arc::ptr_eq(ours, identity))
            || self
                .retained
                .is_some_and(|ours| Arc::ptr_eq(ours, identity))
        {
            return true;
        }
        #[cfg(test)]
        COMPARED_BYTES.with(|count| count.set(count.get() + self.text.len()));
        if self.text == identity.text.as_ref() && same_style(self.style, &identity.style) {
            let _ = self.resolved.set(identity.clone());
            true
        } else {
            false
        }
    }

    fn owned(&self) -> Arc<TextIdentity> {
        if let Some(retained) = self.retained {
            return retained.clone();
        }
        self.resolved
            .get_or_init(|| {
                Arc::new(TextIdentity {
                    text: self.text.into(),
                    style: self.style.clone(),
                    fingerprint: self.fingerprint,
                })
            })
            .clone()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BucketKey {
    fingerprint: u64,
    width: Option<u32>,
    generation: u64,
    scale: u32,
    distinguish_everywhere: bool,
    hyphenator_version: Option<u64>,
}

#[derive(Clone, Copy)]
pub(super) struct CacheKey<'a> {
    input: &'a CacheQuery<'a>,
    bucket: BucketKey,
}

impl CacheKey<'_> {
    pub fn uncached_shape(&self) -> Option<&super::ShapedText> {
        self.input
            .uncached_shape
            .get()
            .filter(|(bucket, _)| *bucket == self.bucket)
            .map(|(_, shaped)| shaped)
    }

    pub fn remember_uncached_shape(&self, shaped: super::ShapedText) {
        let _ = self.input.uncached_shape.set((self.bucket, shaped));
    }
}

struct Entry<V> {
    identity: Arc<TextIdentity>,
    value: V,
    bytes: usize,
}

struct Bucket<V> {
    entries: Vec<Entry<V>>,
    referenced: bool,
}

/// A second-chance clock with one queue record per bucket, never per hit. New
/// entries start cold; a lookup gives them a second chance, protecting repeatedly
/// used labels from streams of one-off probes. Colliding siblings share recency
/// and evict together. This is intentionally approximate recency, not strict LRU.
/// Payload bytes (owned identity + value buffers) and entry count are both bounded;
/// table/queue/bucket metadata is bounded by the entry limit. Shared identities are
/// charged per entry, conservatively. External Arc owners and custom hyphenator
/// internals belong to their callers, and cannot be bounded by cache eviction.
pub(super) struct TextCache<V> {
    buckets: HashMap<BucketKey, Bucket<V>>,
    clock: VecDeque<BucketKey>,
    entries: usize,
    bytes: usize,
    entry_limit: usize,
    byte_limit: usize,
}

impl<V> TextCache<V> {
    pub fn new(entry_limit: usize, byte_limit: usize) -> Self {
        Self {
            buckets: HashMap::new(),
            clock: VecDeque::new(),
            entries: 0,
            bytes: 0,
            entry_limit,
            byte_limit,
        }
    }

    pub fn get(&mut self, key: &CacheKey<'_>) -> Option<&V> {
        let bucket = self.buckets.get_mut(&key.bucket)?;
        let entry = bucket
            .entries
            .iter_mut()
            .find(|entry| key.input.matches(&entry.identity))?;
        // A legacy borrowed call may have populated this entry first. After exact
        // equality, retain the caller's immutable handle so future prepared calls
        // skip payload comparisons too. Keep both identities in the current query
        // (resolved + retained) for the paired measure/shape lookup. Extra spare
        // capacity in a caller's style must not defeat the payload budget.
        if let Some(retained) = key.input.retained
            && !Arc::ptr_eq(retained, &entry.identity)
        {
            let old_bytes = entry.identity.bytes();
            let new_bytes = retained.bytes();
            let total = self
                .bytes
                .saturating_sub(old_bytes)
                .saturating_add(new_bytes);
            if total <= self.byte_limit {
                self.bytes = total;
                entry.bytes = entry.bytes - old_bytes + new_bytes;
                entry.identity = retained.clone();
            }
        }
        bucket.referenced = true;
        Some(&entry.value)
    }

    pub fn contains_key(&mut self, key: &CacheKey<'_>) -> bool {
        self.get(key).is_some()
    }

    pub fn insert(&mut self, key: CacheKey<'_>, value: V, value_bytes: usize) -> bool {
        if self.contains_key(&key) {
            return true;
        }
        if self.entry_limit == 0 || value_bytes > self.byte_limit {
            return false;
        }
        let identity = key.input.owned();
        let bytes = identity.bytes().saturating_add(value_bytes);
        // Oversized one-off paragraphs must not flush the resident working set.
        if bytes > self.byte_limit {
            return false;
        }
        while self.entries >= self.entry_limit || self.bytes > self.byte_limit - bytes {
            self.evict();
        }
        let bucket = self.buckets.entry(key.bucket).or_insert_with(|| {
            self.clock.push_back(key.bucket);
            Bucket {
                entries: Vec::new(),
                referenced: false,
            }
        });
        bucket.entries.push(Entry {
            identity,
            value,
            bytes,
        });
        self.entries += 1;
        self.bytes += bytes;
        true
    }

    fn evict(&mut self) {
        while let Some(key) = self.clock.pop_front() {
            let hash_map::Entry::Occupied(mut bucket) = self.buckets.entry(key) else {
                continue;
            };
            if bucket.get().referenced {
                bucket.get_mut().referenced = false;
                self.clock.push_back(key);
            } else {
                let bucket = bucket.remove();
                self.entries -= bucket.entries.len();
                for entry in bucket.entries {
                    self.bytes -= entry.bytes;
                }
                return;
            }
        }
    }

    pub fn clear(&mut self) {
        self.buckets.clear();
        self.clock.clear();
        self.entries = 0;
        self.bytes = 0;
    }
}

#[cfg(test)]
thread_local! {
    static HASHED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static COMPARED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
