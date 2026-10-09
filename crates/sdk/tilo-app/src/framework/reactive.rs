//! Which bound controls a change wakes.
//!
//! A control bound to a path declares what it reads; after a change only the
//! readers whose paths the change touched run. A reader bound with an
//! arbitrary closure runs after every change, so correctness never depends
//! on tracking. Readers are kept in a trie of path segments, walked against
//! the change's [`Delta`] from the root: the walk descends only into what the
//! change touched, so a one-field edit in a collection of a thousand items
//! wakes the readers of that one field, not a thousand.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::{Rc, Weak};

use tilo_resource::__private::{ObjId, ROOT, ReadDoc as _, Value};
use tilo_resource::shape::{ENTRY_ORDER_KEY, ENTRY_VALUE_KEY};
use tilo_resource::{Delta, Doc, Resource, Segment, Touch};

/// What a reader reads.
pub(crate) enum Reads {
    /// Anything: it runs after every change.
    Always,
    /// The value at a path, from the record's root: it runs when the change
    /// touched that value, something inside it, or something holding it.
    At(Vec<Segment>),
    /// Which items a collection holds and in what order: it runs when an
    /// item was added, removed or moved, not when one changed inside.
    Members(Vec<Segment>),
}

/// The change readers follow.
pub(crate) enum Change<'a> {
    /// Unknown: every reader runs.
    Everything,
    /// What a delta of `doc` touched.
    Delta { delta: &'a Delta, doc: &'a Doc },
}

type Update<R> = Box<dyn FnMut(&<R as Resource>::State)>;

struct Entry<R: Resource> {
    alive: Weak<()>,
    update: Update<R>,
}

#[derive(Default)]
struct Node {
    /// Readers of the value at this node.
    here: Vec<u64>,
    /// Readers of the membership of the collection at this node.
    members: Vec<u64>,
    children: HashMap<Segment, Node>,
}

impl Node {
    fn at(&mut self, path: &[Segment]) -> &mut Self {
        path.iter().fold(self, |node, segment| {
            node.children.entry(segment.clone()).or_default()
        })
    }

    /// Every reader at or below this node.
    fn all(&self, out: &mut BTreeSet<u64>) {
        out.extend(&self.here);
        out.extend(&self.members);
        for child in self.children.values() {
            child.all(out);
        }
    }

    /// Forget readers that no longer exist, and nodes left empty.
    fn prune(&mut self, live: &BTreeMap<u64, impl Sized>) -> bool {
        self.here.retain(|id| live.contains_key(id));
        self.members.retain(|id| live.contains_key(id));
        self.children.retain(|_, child| !child.prune(live));
        self.here.is_empty() && self.members.is_empty() && self.children.is_empty()
    }

    /// The readers of what `delta` touched below `obj`, the object this node
    /// stands for.
    fn touched(&self, obj: &ObjId, delta: &Delta, doc: &Doc, out: &mut BTreeSet<u64>) {
        let touched = delta.under(obj);
        if touched.is_all() {
            for child in self.children.values() {
                child.all(out);
            }
            return;
        }
        if touched.is_empty() {
            return;
        }
        for (segment, child) in &self.children {
            match segment {
                Segment::Field(key) | Segment::Key(key) => match touched.key(key) {
                    Touch::Untouched => {}
                    Touch::Written => child.all(out),
                    Touch::Below => child.below(obj, key, delta, doc, out),
                },
                Segment::Item(id) => match touched.key(id.as_str()) {
                    Touch::Untouched => {}
                    Touch::Written => child.all(out),
                    Touch::Below => {
                        // An item is stored as an entry holding its order key
                        // and its value; a move touches only the order key.
                        let Some(entry) = object(doc, obj, id.as_str()) else {
                            child.all(out);
                            continue;
                        };
                        match delta.key(&entry, ENTRY_VALUE_KEY) {
                            Touch::Untouched => {}
                            Touch::Written => child.all(out),
                            Touch::Below => child.below(&entry, ENTRY_VALUE_KEY, delta, doc, out),
                        }
                    }
                },
                // A grid's cells have a layout of their own; any change to
                // the grid wakes its cells' readers.
                Segment::Cell { .. } => child.all(out),
            }
        }
    }

    /// This node, whose slot `key` of `parent` holds an object something
    /// inside of which changed.
    fn below(&self, parent: &ObjId, key: &str, delta: &Delta, doc: &Doc, out: &mut BTreeSet<u64>) {
        out.extend(&self.here);
        let Some(obj) = object(doc, parent, key) else {
            self.all(out);
            return;
        };
        if !self.members.is_empty() && membership_changed(&obj, delta, doc) {
            out.extend(&self.members);
        }
        self.touched(&obj, delta, doc, out);
    }
}

/// The object in slot `key` of `parent`.
fn object(doc: &Doc, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(_), id))) => Some(id),
        _ => None,
    }
}

/// Whether the collection `obj` gained, lost or reordered items.
fn membership_changed(obj: &ObjId, delta: &Delta, doc: &Doc) -> bool {
    let touched = delta.under(obj);
    if touched.is_all() || touched.written().next().is_some() {
        return true;
    }
    touched.keys().any(|item| {
        object(doc, obj, item)
            .is_none_or(|entry| delta.key(&entry, ENTRY_ORDER_KEY) != Touch::Untouched)
    })
}

/// How many readers a change may cost on average for sweeping out dead ones.
const SWEEP_EVERY: usize = 16;

struct Index<R: Resource> {
    next: u64,
    /// Changes followed since dead readers were last swept out.
    since_sweep: usize,
    entries: BTreeMap<u64, Entry<R>>,
    always: Vec<u64>,
    root: Node,
}

/// The readers bound controls registered, each tied to the lifetime of the
/// view scope that created it (a row of an [`Each`](super::Each)).
pub(crate) struct Updaters<R: Resource>(Rc<RefCell<Index<R>>>);

impl<R: Resource> Clone for Updaters<R> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<R: Resource> Updaters<R> {
    pub(crate) fn new() -> Self {
        Self(Rc::new(RefCell::new(Index {
            next: 0,
            since_sweep: 0,
            entries: BTreeMap::new(),
            always: Vec::new(),
            root: Node::default(),
        })))
    }

    pub(crate) fn push(&self, alive: Weak<()>, reads: Reads, update: Update<R>) {
        let mut index = self.0.borrow_mut();
        let id = index.next;
        index.next = index.next.saturating_add(1);
        index.entries.insert(id, Entry { alive, update });
        match reads {
            Reads::Always => index.always.push(id),
            Reads::At(path) => index.root.at(&path).here.push(id),
            Reads::Members(path) => index.root.at(&path).members.push(id),
        }
    }

    /// Run the readers `change` wakes, in the order they were registered.
    /// A reader may register more (a rebuilt row); those run from the next
    /// change on.
    pub(crate) fn run(&self, state: &R::State, change: &Change<'_>) {
        let woken: BTreeSet<u64> = {
            let mut index = self.0.borrow_mut();
            let index = &mut *index;
            // Readers of rows and parts that are gone are dropped when they
            // would run; the whole index is swept only now and then, so a
            // change costs what it wakes, not what the view holds.
            index.since_sweep = index.since_sweep.saturating_add(1);
            if index.since_sweep.saturating_mul(SWEEP_EVERY) >= index.entries.len() {
                index.since_sweep = 0;
                index
                    .entries
                    .retain(|_, entry| entry.alive.strong_count() > 0);
                let entries = &index.entries;
                index.always.retain(|id| entries.contains_key(id));
                index.root.prune(entries);
            }
            let entries = &index.entries;
            match change {
                Change::Everything => entries.keys().copied().collect(),
                Change::Delta { delta, doc } => {
                    let mut woken: BTreeSet<u64> = index.always.iter().copied().collect();
                    if delta.is_everything() {
                        woken.extend(entries.keys().copied());
                    } else if !delta.is_empty() {
                        woken.extend(&index.root.here);
                        index.root.touched(&ROOT, delta, doc, &mut woken);
                    }
                    woken
                }
            }
        };
        for id in woken {
            let Some(mut entry) = self.0.borrow_mut().entries.remove(&id) else {
                continue;
            };
            if entry.alive.strong_count() == 0 {
                continue;
            }
            {
                #[cfg(feature = "test-support")]
                let _profile = super::profiling::Scope::start("framework.reader");
                (entry.update)(state);
            }
            self.0.borrow_mut().entries.insert(id, entry);
        }
    }
}
