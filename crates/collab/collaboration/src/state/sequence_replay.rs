//! Where a diff's sequence patches land in the live document.
//!
//! A diff indexes each list or text patch against the sequence as the patches
//! before it in the same diff left it, starting from the state being reverted.
//! Replaying several patches into one object therefore has to follow that
//! running sequence: a delete after an insert of the same diff names the
//! position the insert moved things to, not the one the reverted state had.
//!
//! When the live object still reads exactly as the reverted state did — the
//! usual case, including after later actions were undone, which rewrites
//! equal content under new identities — the patches apply to it as they are.
//! Otherwise the live document holds edits merged in since, which the replay
//! must neither lose nor mistake for the elements a patch names; the replay
//! then keeps both sides as element identities (cursors): the running sequence
//! the diff describes, and the live sequence as the replay writes it. A patch
//! index is resolved through the first, and the elements it names are found
//! in the second — or found gone, when a collaborator removed them since.
//! Every lookup is one pass over the object's elements, whatever the number
//! of elements a patch names.

use std::collections::{HashMap, HashSet};

use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, ReadDoc, Value};

/// An element's identity: its cursor, encoded.
type Key = Vec<u8>;

/// One element of a sequence: its identity, and how many index units it
/// spans (one in a list; its width in the document's text encoding in text).
#[derive(Clone, Debug)]
pub(super) struct Element {
    /// `None` for an element the diff counts but the live document never
    /// received, because the replay skipped the patch that inserts it.
    key: Option<Key>,
    width: usize,
}

/// The elements of `obj` from unit `from` for `units` units, as of `heads`
/// (`None`: live).
fn elements(
    doc: &AutoCommit,
    obj: &ObjId,
    from: usize,
    units: usize,
    heads: Option<&[ChangeHash]>,
) -> Vec<Element> {
    let mut out: Vec<Element> = Vec::new();
    for unit in from..from.saturating_add(units) {
        let key = doc
            .get_cursor(obj, unit, heads)
            .ok()
            .map(|cursor| cursor.to_bytes());
        // A character wider than one unit answers every unit it covers.
        match out.last_mut() {
            Some(last) if key.is_some() && last.key == key => {
                last.width = last.width.saturating_add(1);
            }
            _ => out.push(Element { key, width: 1 }),
        }
    }
    out
}

/// Whether `obj` reads live exactly as it did at `anchor`: the same text, or
/// the same values (objects by type) at the same positions.
fn reads_as_at(doc: &AutoCommit, obj: &ObjId, anchor: &[ChangeHash]) -> bool {
    match doc.object_type(obj) {
        Ok(ObjType::Text) => match (doc.text(obj), doc.text_at(obj, anchor)) {
            (Ok(live), Ok(then)) => live == then,
            _ => false,
        },
        Ok(ObjType::List) => {
            let same = |a: &Value<'_>, b: &Value<'_>| match (a, b) {
                (Value::Object(a), Value::Object(b)) => a == b,
                (Value::Scalar(a), Value::Scalar(b)) => a == b,
                _ => false,
            };
            doc.length(obj) == doc.length_at(obj, anchor)
                && doc
                    .values(obj)
                    .zip(doc.values_at(obj, anchor))
                    .all(|((live, _), (then, _))| same(&live, &then))
        }
        _ => false,
    }
}

/// A place in the live sequence: an element index, and its first unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Live {
    pub(super) element: usize,
    pub(super) unit: usize,
}

/// One sequence object's replay state. See the module docs.
#[derive(Debug, Default)]
pub(super) enum SequenceReplay {
    /// The live object is the running sequence: patch indices are live
    /// positions.
    #[default]
    Positional,
    /// The live object holds edits the diff does not know about.
    Identity {
        /// The sequence as the diff's patches so far describe it.
        order: Vec<Element>,
        /// The live sequence, kept in step with every write of the replay.
        live: Vec<Element>,
    },
}

impl SequenceReplay {
    /// The replay of `obj`, whose patches are indexed from `anchor`. Reads
    /// only. A replay that skips some of the object's patches follows
    /// identities even over equal content, since the live object will not
    /// take the skipped steps.
    fn new(doc: &AutoCommit, obj: &ObjId, anchor: &[ChangeHash], skips: bool) -> Self {
        if !skips && reads_as_at(doc, obj, anchor) {
            return Self::Positional;
        }
        Self::Identity {
            order: elements(doc, obj, 0, doc.length_at(obj, anchor), Some(anchor)),
            live: elements(doc, obj, 0, doc.length(obj), None),
        }
    }

    /// The replay of an object the replay itself creates: it starts empty
    /// and takes every patch as written.
    pub(super) fn fresh() -> Self {
        Self::Positional
    }

    /// The running sequence's element at unit `index` (its length when
    /// `index` is past the end).
    fn at(order: &[Element], index: usize) -> usize {
        let mut units = 0usize;
        let mut element = 0;
        while units < index
            && let Some(entry) = order.get(element)
        {
            units = units.saturating_add(entry.width);
            element = element.saturating_add(1);
        }
        element
    }

    /// Every live element of the running sequence's elements `range`, as
    /// `(live place, position in range)`, in live order.
    fn live_of(
        order: &[Element],
        live: &[Element],
        range: std::ops::Range<usize>,
    ) -> Vec<(Live, usize)> {
        let wanted: HashMap<&[u8], usize> = order
            .get(range)
            .unwrap_or(&[])
            .iter()
            .enumerate()
            .filter_map(|(position, e)| Some((e.key.as_deref()?, position)))
            .collect();
        if wanted.is_empty() {
            return Vec::new();
        }
        let mut unit = 0usize;
        let mut found = Vec::new();
        for (element, candidate) in live.iter().enumerate() {
            if let Some(position) = candidate.key.as_deref().and_then(|key| wanted.get(key)) {
                found.push((Live { element, unit }, *position));
            }
            unit = unit.saturating_add(candidate.width);
        }
        found
    }

    fn end_of(live: &[Element], place: Live) -> Live {
        Live {
            element: place.element.saturating_add(1),
            unit: place
                .unit
                .saturating_add(live.get(place.element).map_or(0, |e| e.width)),
        }
    }

    /// The live place of an insertion at unit `index` of the running
    /// sequence: right after the nearest element before it that is still
    /// live, or the start.
    pub(super) fn insertion(&self, index: usize) -> Live {
        match self {
            Self::Positional => Live {
                element: index,
                unit: index,
            },
            Self::Identity { order, live } => {
                let element = Self::at(order, index);
                Self::live_of(order, live, 0..element)
                    .into_iter()
                    .max_by_key(|(_, position)| *position)
                    .map_or_else(Live::default, |(place, _)| Self::end_of(live, place))
            }
        }
    }

    /// Record `units` units the diff inserts at unit `index`, which the
    /// replay wrote live at `at`, reading their identities back from `obj`.
    pub(super) fn inserted(
        &mut self,
        doc: &AutoCommit,
        obj: &ObjId,
        index: usize,
        at: Live,
        units: usize,
    ) {
        let Self::Identity { order, live } = self else {
            return;
        };
        let written = elements(doc, obj, at.unit, units, None);
        let element = Self::at(order, index);
        order.splice(element..element, written.iter().cloned());
        let place = at.element.min(live.len());
        live.splice(place..place, written);
    }

    /// Record `count` list elements the diff inserts at `index` that the
    /// replay does not write.
    pub(super) fn skipped_insert(&mut self, index: usize, count: usize) {
        let Self::Identity { order, .. } = self else {
            return;
        };
        let element = Self::at(order, index);
        order.splice(
            element..element,
            (0..count).map(|_| Element {
                key: None,
                width: 1,
            }),
        );
    }

    /// Record `length` units the diff deletes at unit `index` that the
    /// replay keeps live.
    pub(super) fn skipped_delete(&mut self, index: usize, length: usize) {
        let Self::Identity { order, .. } = self else {
            return;
        };
        let start = Self::at(order, index);
        let end = Self::at(order, index.saturating_add(length)).max(start);
        order.drain(start..end);
    }

    /// Take `length` units at unit `index` out of the running sequence,
    /// returning the live runs they covered — each as its first live place
    /// and its width in units — last first, so deleting them in turn keeps
    /// the places still to come valid. Elements already gone from the live
    /// document are simply dropped. The caller deletes the runs; they are
    /// already gone from the replay's view of the live sequence.
    pub(super) fn delete(&mut self, index: usize, length: usize) -> Vec<(Live, usize)> {
        let Self::Identity { order, live } = self else {
            return vec![(
                Live {
                    element: index,
                    unit: index,
                },
                length,
            )];
        };
        let start = Self::at(order, index);
        let end = Self::at(order, index.saturating_add(length)).max(start);
        let places: Vec<Live> = Self::live_of(order, live, start..end)
            .into_iter()
            .map(|(place, _)| place)
            .collect();
        order.drain(start..end);
        // Contiguous live elements delete as one run.
        let mut runs: Vec<(Live, usize, usize)> = Vec::new();
        for place in places {
            let width = live.get(place.element).map_or(0, |e| e.width);
            match runs.last_mut() {
                Some((first, count, units))
                    if first.element.saturating_add(*count) == place.element =>
                {
                    *count = count.saturating_add(1);
                    *units = units.saturating_add(width);
                }
                _ => runs.push((place, 1, width)),
            }
        }
        runs.into_iter()
            .rev()
            .map(|(first, count, units)| {
                let end = first.element.saturating_add(count).min(live.len());
                live.drain(first.element..end);
                (first, units)
            })
            .collect()
    }

    /// Where the element at unit `index` of the running sequence is live,
    /// if it still is.
    pub(super) fn element(&self, index: usize) -> Option<Live> {
        match self {
            Self::Positional => Some(Live {
                element: index,
                unit: index,
            }),
            Self::Identity { order, live } => {
                let element = Self::at(order, index);
                Self::live_of(order, live, element..element.saturating_add(1))
                    .first()
                    .map(|(place, _)| *place)
            }
        }
    }

    /// The element at unit `index` of the running sequence was overwritten
    /// live at `at`: follow it to its new identity.
    pub(super) fn replaced(&mut self, doc: &AutoCommit, obj: &ObjId, index: usize, at: Live) {
        let Self::Identity { order, live } = self else {
            return;
        };
        let key = doc
            .get_cursor(obj, at.unit, None)
            .ok()
            .map(|cursor| cursor.to_bytes());
        let element = Self::at(order, index);
        if let Some(entry) = order.get_mut(element) {
            entry.key.clone_from(&key);
        }
        if let Some(entry) = live.get_mut(at.element) {
            entry.key = key;
        }
    }

    /// The live unit range the running sequence's units `[start, end)`
    /// cover: from the first of their elements still live to the end of
    /// the last. `None` when none is.
    pub(super) fn span(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        let Self::Identity { order, live } = self else {
            return Some((start, end));
        };
        let found = Self::live_of(order, live, Self::at(order, start)..Self::at(order, end));
        let (first, _) = found.first()?;
        let (last, _) = found.last()?;
        Some((first.unit, Self::end_of(live, *last).unit))
    }
}

/// The sequence objects a patch stream edits in place — lists and text it
/// does not itself recreate — each with its replay state, read before the
/// replay writes anything.
pub(super) fn prepare(
    doc: &AutoCommit,
    patches: &[(automerge::Patch, bool)],
    anchor: &[ChangeHash],
) -> HashMap<ObjId, SequenceReplay> {
    use automerge::PatchAction;
    let mut recreated: HashSet<&ObjId> = HashSet::new();
    // Each edited object, and whether any of its patches is skipped.
    let mut edited: HashMap<&ObjId, bool> = HashMap::new();
    for (patch, apply) in patches {
        match &patch.action {
            PatchAction::PutMap { value, .. } | PatchAction::PutSeq { value, .. } => {
                if matches!(value.0, Value::Object(_)) {
                    recreated.insert(&value.1);
                }
            }
            PatchAction::Insert { values, .. } => {
                recreated.extend(
                    values
                        .iter()
                        .filter(|(value, _, _)| matches!(value, Value::Object(_)))
                        .map(|(_, id, _)| id),
                );
            }
            _ => {}
        }
        *edited.entry(&patch.obj).or_default() |= !apply;
    }
    edited
        .into_iter()
        .filter(|(obj, _)| !recreated.contains(obj))
        .filter(|(obj, _)| matches!(doc.object_type(obj), Ok(ObjType::List | ObjType::Text)))
        .map(|(obj, skips)| (obj.clone(), SequenceReplay::new(doc, obj, anchor, skips)))
        .collect()
}
