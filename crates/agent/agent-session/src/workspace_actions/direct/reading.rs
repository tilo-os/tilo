//! What one document keeps of direct conversations, read again after it changed.
//!
//! The service answers every call into the app that keeps conversations by what its documents
//! keep of direct ones ([`super`]). Read whole after each change, a document costs every word
//! of a growing reply a walk through all its conversation holds. So a document that is
//! written to is kept here as a copy with its entries counted, and a change moves only the
//! entries it touched: the copy takes the changes the document took since, and the document's
//! own account of what they touched says which entries to read again, as they stood before
//! and as they stand now.
//!
//! What following finds is what [`said_directly`] finds in the document whole, and where a
//! change cannot be followed the document is read whole again.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use automerge::{ChangeHash, ObjId, ObjType, Patch, PatchAction, Prop, ReadDoc, Value};
use collab_types::DocumentMutation;
use ui::permissions::MemberId;

use super::{DocumentRoute, ITEM_VALUE, MOST_DEPTH, Walk, WorkspaceState, said_directly};

/// The direct conversation an object names, and the id it keeps of itself ([`Walk::names`]).
type Names = (Option<String>, Option<String>);

/// Where each object a change touched is kept, and how deep in its document.
type Places = HashMap<ObjId, (Option<(ObjId, Prop)>, usize)>;

/// What one document keeps of direct conversations as it was last read, and what reading it
/// again takes.
pub(in super::super) struct Reading {
    /// Every entry it keeps of a direct conversation, as [`said_directly`] finds them.
    said: Arc<[(String, String)]>,
    /// The heads it stood at then, as the workspace hands them out.
    heads: Vec<[u8; 32]>,
    /// The document itself with its entries counted, from its second reading on: one that is
    /// read once and not written again leaves only what was found in it.
    followed: Option<Followed>,
}

impl Reading {
    /// Every entry the document keeps of a direct conversation, as last read.
    pub(super) fn said(&self) -> Arc<[(String, String)]> {
        Arc::clone(&self.said)
    }

    /// The document at `route` read again. `before` is its last reading: it is brought up to
    /// where the document stands by the changes it took since, which costs those changes and
    /// not the document. It is read whole the first time, the first time it changed after
    /// that (a document nobody writes to is not kept here), and wherever it cannot be
    /// followed. A document that does not read keeps what it kept, and nothing before it read.
    pub(super) fn again(
        before: Option<Self>,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Self {
        let written = before.is_some();
        let whole = match before {
            Some(mut read) => match state.update(route, Some(&read.heads)) {
                Ok(None) => return read,
                Ok(Some((DocumentMutation::Incremental(changes), heads))) => {
                    if read.follow(&changes, &heads).is_some() {
                        return read;
                    }
                    None
                }
                Ok(Some((DocumentMutation::Snapshot(bytes), heads))) => Some((bytes, heads)),
                Err(_) => None,
            },
            None => None,
        };
        match whole.map_or_else(|| state.document(route), Ok) {
            Ok((bytes, heads)) => Self::whole(&bytes, heads, written),
            Err(_) => Self {
                said: said_directly(&[]),
                heads: Vec::new(),
                followed: None,
            },
        }
    }

    /// The document saved as `bytes`, which stands at `heads`, read whole; kept to be
    /// followed where `keep`.
    fn whole(bytes: &[u8], heads: Vec<[u8; 32]>, keep: bool) -> Self {
        let mut walk = Walk::default();
        let Ok(mut doc) = automerge::AutoCommit::load(bytes) else {
            return Self {
                said: walk.found.into(),
                heads,
                followed: None,
            };
        };
        walk.gather(&doc, None, (&automerge::ROOT, ObjType::Map, 0));
        let followed = keep.then(|| {
            // From here the copy indexes what each change touches.
            doc.update_diff_cursor();
            let mut found = HashMap::new();
            for entry in &walk.found {
                let count = found.entry(entry.clone()).or_insert(0_usize);
                *count = count.saturating_add(1);
            }
            Followed { doc, found }
        });
        Self {
            said: walk.found.into(),
            heads,
            followed,
        }
    }

    /// Bring this reading up to the document by `changes`, the ones it took since, after
    /// which it stands at `heads`: how many objects that read. `None` where they cannot be
    /// followed: the copy is let go, and the document is read whole.
    fn follow(&mut self, changes: &[u8], heads: &[[u8; 32]]) -> Option<usize> {
        let mut followed = self.followed.take()?;
        let read = followed.take(changes, heads)?;
        if read.moved {
            self.said = followed.found.keys().cloned().collect();
        }
        self.heads = heads.to_vec();
        self.followed = Some(followed);
        Some(read.objects)
    }
}

/// A document the service keeps a copy of, to read what a change moved of its entries of
/// direct conversations instead of reading all of it again.
struct Followed {
    /// The copy, indexing what each change touches.
    doc: automerge::AutoCommit,
    /// How often each entry is found in it: one found twice stays when one of the two goes.
    found: HashMap<(String, String), usize>,
}

/// What following a change read, and whether it moved what is found.
struct Moved {
    objects: usize,
    moved: bool,
}

/// What of a document its changes touched.
#[derive(Default)]
struct Touched {
    places: Places,
    /// Entries of maps a change put, replaced or took away: the map and the key.
    cells: HashSet<(ObjId, String)>,
    /// Lists a change put into or took from: their places moved, so each is read whole.
    lists: HashSet<ObjId>,
    /// Objects one of whose own strings changed, which may name them anew.
    frames: Vec<ObjId>,
}

impl Touched {
    /// Whether `object` is kept in a list that is read whole, and so is read with it.
    fn in_a_list_read_whole(&self, object: &ObjId) -> bool {
        let mut at = object;
        while let Some((Some((parent, _)), _)) = self.places.get(at) {
            if self.lists.contains(parent) {
                return true;
            }
            at = parent;
        }
        false
    }
}

impl Followed {
    /// Take `changes` into the copy, after which the document stands at `heads`, and move
    /// what is found by what they touched.
    fn take(&mut self, changes: &[u8], heads: &[[u8; 32]]) -> Option<Moved> {
        let before = self.doc.get_heads();
        if before.is_empty() || self.doc.diff_cursor() != before {
            return None;
        }
        tilo_automerge::ApplyIncoming::load_incoming(&mut self.doc, changes).ok()?;
        let after = self.doc.get_heads();
        let mut stands: Vec<[u8; 32]> = after.iter().map(|head| head.0).collect();
        stands.sort_unstable();
        let mut asked = heads.to_vec();
        asked.sort_unstable();
        if stands != asked {
            return None;
        }
        let patches = self.doc.diff(&before, &after);
        self.doc.update_diff_cursor();
        self.moved_by(&patches, &before)
    }

    /// What `patches`, the copy's own account of what it took since it stood at `before`,
    /// touched: the entries of maps, the lists, and the objects whose own strings changed.
    /// An object a change made is left out: it is read with the entry it was put under.
    fn touched(&self, patches: &[Patch], before: &[ChangeHash]) -> Option<Touched> {
        let mut touched = Touched::default();
        touched.places.insert(automerge::ROOT, (None, 0));
        for patch in patches {
            for (index, (parent, prop)) in patch.path.iter().enumerate() {
                let below = index.saturating_add(1);
                let child = patch.path.get(below).map_or(&patch.obj, |(next, _)| next);
                touched
                    .places
                    .entry(child.clone())
                    .or_insert_with(|| (Some((parent.clone(), prop.clone())), below));
            }
            let stood = match patch.path.last() {
                None => true,
                Some((parent, prop)) => self
                    .doc
                    .get_at(parent, prop.clone(), before)
                    .ok()?
                    .is_some_and(|(value, id)| value.is_object() && id == patch.obj),
            };
            if !stood {
                continue;
            }
            match &patch.action {
                PatchAction::PutMap { key, .. }
                | PatchAction::DeleteMap { key }
                | PatchAction::Conflict {
                    prop: Prop::Map(key),
                } => {
                    touched.cells.insert((patch.obj.clone(), key.clone()));
                }
                PatchAction::PutSeq { .. }
                | PatchAction::Insert { .. }
                | PatchAction::DeleteSeq { .. }
                | PatchAction::Conflict { prop: Prop::Seq(_) } => {
                    touched.lists.insert(patch.obj.clone());
                }
                PatchAction::SpliceText { .. }
                | PatchAction::Mark { .. }
                | PatchAction::Increment { .. } => {}
            }
        }
        for (object, key) in &touched.cells {
            let text =
                |value: Option<(Value<'_>, ObjId)>| value.is_some_and(|(value, _)| value.is_str());
            let was = text(self.doc.get_at(object, key.as_str(), before).ok()?);
            let is = text(self.doc.get(object, key.as_str()).ok()?);
            if key == ITEM_VALUE || was || is {
                touched.frames.push(object.clone());
            }
        }
        Some(touched)
    }

    /// What each object whose own strings changed named before and names now. One that is
    /// named anew names the entry it is kept under anew, or, where it holds an item's own
    /// fields, the item: so on, up to where nothing more is named anew.
    fn renamed(
        &self,
        touched: &mut Touched,
        before: &[ChangeHash],
        walk: &mut Walk,
    ) -> Option<HashMap<ObjId, [Names; 2]>> {
        let mut named = HashMap::new();
        while let Some(object) = touched.frames.pop() {
            if named.contains_key(&object) {
                continue;
            }
            let (place, depth) = touched.places.get(&object)?.clone();
            let kind = self.doc.object_type(&object).ok()?;
            let was = walk.names(&self.doc, Some(before), (&object, kind, depth));
            let is = walk.names(&self.doc, None, (&object, kind, depth));
            if was.0 != is.0
                && let Some((parent, Prop::Map(key))) = place
            {
                if key == ITEM_VALUE {
                    touched.frames.push(parent);
                } else {
                    touched.cells.insert((parent, key));
                }
            }
            named.insert(object, [was, is]);
        }
        Some(named)
    }

    /// Move what is found by what `patches` touched since the copy stood at `before`: each
    /// touched list, entry and object is read as it stood then and as it stands now, what it
    /// put into what is found then is taken out and what it puts in now is put in. `None`
    /// where the two do not add up, which a whole reading sets right.
    fn moved_by(&mut self, patches: &[Patch], before: &[ChangeHash]) -> Option<Moved> {
        let mut touched = self.touched(patches, before)?;
        let mut walk = Walk::default();
        let named = self.renamed(&mut touched, before, &mut walk)?;
        let mut moved = false;
        for list in &touched.lists {
            if touched.in_a_list_read_whole(list) {
                continue;
            }
            let depth = touched.places.get(list)?.1;
            let kind = self.doc.object_type(list).ok()?;
            let (mut was, mut is) = (Walk::default(), Walk::default());
            was.gather(&self.doc, Some(before), (list, kind, depth));
            is.gather(&self.doc, None, (list, kind, depth));
            walk.read = walk.read.saturating_add(was.read).saturating_add(is.read);
            moved |= self.exchange(was.found, is.found)?;
        }
        for (object, key) in &touched.cells {
            if touched.in_a_list_read_whole(object) {
                continue;
            }
            let depth = touched.places.get(object)?.1;
            let (was, is) = self.entry((object, key, depth), before, &named)?;
            walk.read = walk.read.saturating_add(was.read).saturating_add(is.read);
            moved |= self.exchange(was.found, is.found)?;
        }
        for (object, [was, is]) in &named {
            if touched.in_a_list_read_whole(object) {
                continue;
            }
            // An object that keeps its own id is an entry of what it names, under that id.
            let own = |(conversation, id): &Names| id.clone().zip(conversation.clone());
            let (was, is) = (own(was), own(is));
            moved |= self.exchange(was.into_iter().collect(), is.into_iter().collect())?;
        }
        Some(Moved {
            objects: walk.read,
            moved,
        })
    }

    /// What the entry `key` of the map `object`, `depth` deep, put into what is found before
    /// the change, and what it puts there now: its own key where that is a direct
    /// conversation, the conversation its object names, and everything below an object the
    /// change put there or took away. An object that stayed is read no further: what a
    /// change moved in it is an entry of its own, and `named` says whether it is named anew.
    fn entry(
        &self,
        (object, key, depth): (&ObjId, &str, usize),
        before: &[ChangeHash],
        named: &HashMap<ObjId, [Names; 2]>,
    ) -> Option<(Walk, Walk)> {
        let (mut was, mut is) = (Walk::default(), Walk::default());
        if depth > MOST_DEPTH {
            return Some((was, is));
        }
        let object_of = |value: Option<(Value<'_>, ObjId)>| match value {
            Some((Value::Object(kind), id)) => (true, Some((id, kind))),
            Some(_) => (true, None),
            None => (false, None),
        };
        let (stood, old) = object_of(self.doc.get_at(object, key, before).ok()?);
        let (stands, new) = object_of(self.doc.get(object, key).ok()?);
        if MemberId::direct_conversation_members(key).is_some() {
            if stood {
                was.found.push((key.to_owned(), key.to_owned()));
            }
            if stands {
                is.found.push((key.to_owned(), key.to_owned()));
            }
        }
        let below = depth.saturating_add(1);
        let (named_then, named_now) = match (&old, &new) {
            (Some((gone, _)), Some((kept, _))) if gone == kept => named
                .get(kept)
                .map_or((None, None), |[then, now]| (then.0.clone(), now.0.clone())),
            _ => (
                old.as_ref()
                    .and_then(|(id, kind)| was.gather(&self.doc, Some(before), (id, *kind, below))),
                new.as_ref()
                    .and_then(|(id, kind)| is.gather(&self.doc, None, (id, *kind, below))),
            ),
        };
        if key != ITEM_VALUE {
            was.found
                .extend(named_then.map(|conversation| (key.to_owned(), conversation)));
            is.found
                .extend(named_now.map(|conversation| (key.to_owned(), conversation)));
        }
        Some((was, is))
    }

    /// Take `gone` out of what is found and put `come` in: whether that moved anything.
    /// `None` where something gone was never found.
    fn exchange(
        &mut self,
        mut gone: Vec<(String, String)>,
        mut come: Vec<(String, String)>,
    ) -> Option<bool> {
        gone.sort_unstable();
        come.sort_unstable();
        if gone == come {
            return Some(false);
        }
        for entry in gone {
            let count = self.found.get_mut(&entry)?;
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.found.remove(&entry);
            }
        }
        for entry in come {
            let count = self.found.entry(entry).or_insert(0);
            *count = count.saturating_add(1);
        }
        Some(true)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use automerge::transaction::Transactable as _;

    use super::super::OWN_ID;
    use super::super::tests::{between, saved};
    use super::*;

    fn heads_of(doc: &mut automerge::AutoCommit) -> Vec<[u8; 32]> {
        let mut heads: Vec<[u8; 32]> = doc.get_heads().iter().map(|head| head.0).collect();
        heads.sort_unstable();
        heads
    }

    fn as_set(said: &[(String, String)]) -> BTreeSet<(String, String)> {
        said.iter().cloned().collect()
    }

    /// A document that is written to, and the reading the service keeps of it.
    struct Written {
        doc: automerge::AutoCommit,
        read: Reading,
    }

    impl Written {
        fn of(bytes: &[u8]) -> Self {
            let mut doc = automerge::AutoCommit::load(bytes).unwrap();
            let read = Reading::whole(bytes, heads_of(&mut doc), true);
            Self { doc, read }
        }

        /// The object kept under `key` in `of`.
        fn child(&self, of: &ObjId, key: &str) -> ObjId {
            self.doc.get(of, key).unwrap().unwrap().1
        }

        /// The own fields of the message `id`.
        fn message(&self, id: &str) -> ObjId {
            let messages = self.child(&automerge::ROOT, "messages");
            self.child(&self.child(&messages, id), ITEM_VALUE)
        }

        /// Hand the reading what the document took since it was last handed any, as the
        /// workspace hands it over: how many objects following that read. What the reading
        /// then keeps is what a whole reading of the document finds.
        fn follow(&mut self, what: &str) -> usize {
            let held: Vec<ChangeHash> = self
                .read
                .heads
                .iter()
                .map(|head| ChangeHash(*head))
                .collect();
            let mut changes = Vec::new();
            for change in self.doc.get_changes(&held) {
                changes.extend_from_slice(change.raw_bytes());
            }
            let heads = heads_of(&mut self.doc);
            let read = self
                .read
                .follow(&changes, &heads)
                .unwrap_or_else(|| panic!("{what}: the reading follows the change"));
            assert_eq!(
                as_set(&self.read.said),
                as_set(&said_directly(&self.doc.save())),
                "{what}: following finds what a whole reading finds"
            );
            read
        }
    }

    /// How many objects are read to follow one more message of a direct conversation and a
    /// word more of it, in a document that holds `messages` of them.
    fn read_for_one_more_message(messages: usize) -> usize {
        let hers = between(&[MemberId::person("ada"), MemberId::agent("scout")]);
        let ids: Vec<String> = (0..messages).map(|at| format!("m-{at}")).collect();
        let held: Vec<(&str, &str)> = ids.iter().map(|id| (id.as_str(), hers.as_str())).collect();
        let mut written = Written::of(&saved(&held, &[]));
        let messages = written.child(&automerge::ROOT, "messages");
        let entry = written
            .doc
            .put_object(&messages, "m-new", ObjType::Map)
            .unwrap();
        written.doc.put(&entry, "k", "a1").unwrap();
        let message = written
            .doc
            .put_object(&entry, ITEM_VALUE, ObjType::Map)
            .unwrap();
        written.doc.put(&message, "said_in", hers.as_str()).unwrap();
        written.doc.put(&message, "text", "one more").unwrap();
        let for_the_message = written.follow("one more message");
        assert!(
            written.read.said.iter().any(|(key, _)| key == "m-new"),
            "the new message is kept"
        );
        // And a word more of it, as a growing reply takes one.
        written.doc.put(&message, "text", "one more word").unwrap();
        for_the_message.saturating_add(written.follow("a word more of it"))
    }

    #[test]
    fn one_more_word_reads_as_much_of_a_long_conversation_as_of_a_new_one() {
        let (new, long) = (
            read_for_one_more_message(20),
            read_for_one_more_message(600),
        );
        assert_eq!(
            new, long,
            "a message and a word more of it read as many objects of a document that holds \
             600 messages as of one that holds 20"
        );
    }

    #[test]
    fn following_a_document_keeps_what_reading_it_whole_keeps_whatever_changed() {
        let hers = between(&[MemberId::person("ada"), MemberId::agent("scout")]);
        let his = between(&[MemberId::person("bo"), MemberId::agent("scout")]);
        let mut written = Written::of(&saved(
            &[("m-hers", hers.as_str()), ("m-open", "general")],
            &[("l-hers", hers.as_str()), ("l-open", "general")],
        ));
        let messages = written.child(&automerge::ROOT, "messages");
        let pinned = written.child(&automerge::ROOT, "pinned");
        let listed = written.child(&automerge::ROOT, "listed");
        let kept = |written: &Written, key: &str| written.read.said.iter().any(|(k, _)| k == key);
        assert!(kept(&written, "m-hers") && !kept(&written, "m-open"));

        let first = written.message("m-hers");
        written.doc.put(&first, "said_in", "general").unwrap();
        written.follow("a message moves to a conversation of everyone");
        assert!(!kept(&written, "m-hers"), "it is no longer kept");
        written.doc.put(&first, "said_in", his.as_str()).unwrap();
        written.follow("and on to another direct conversation");
        assert!(kept(&written, "m-hers"), "it is kept again");
        written.doc.put(&first, "text", "other words").unwrap();
        written.follow("its words change");
        let open = written.message("m-open");
        written.doc.put(&open, "said_in", hers.as_str()).unwrap();
        written.follow("an open message becomes a direct one");

        let entry = written
            .doc
            .put_object(&messages, "m-new", ObjType::Map)
            .unwrap();
        let fields = written
            .doc
            .put_object(&entry, ITEM_VALUE, ObjType::Map)
            .unwrap();
        written.doc.put(&fields, "said_in", hers.as_str()).unwrap();
        written.doc.delete(&pinned, "p-m-hers").unwrap();
        written.follow("a message is added and a pinned entry taken away at once");
        assert!(kept(&written, "m-new") && !kept(&written, "p-m-hers"));
        written.doc.delete(&messages, "m-new").unwrap();
        written.follow("a message is taken away");
        assert!(!kept(&written, "m-new"));

        let item = written.doc.get(&listed, 0_usize).unwrap().unwrap().1;
        written.doc.put(&item, "said_in", "general").unwrap();
        written.follow("an item of a list moves to a conversation of everyone");
        assert!(!kept(&written, "l-hers"));
        let put = written.doc.insert_object(&listed, 0, ObjType::Map).unwrap();
        written.doc.put(&put, OWN_ID, "l-his").unwrap();
        written.doc.put(&put, "said_in", his.as_str()).unwrap();
        written.doc.put(&item, "said_in", hers.as_str()).unwrap();
        written.follow("an item is put into a list while another of it changes");
        assert!(kept(&written, "l-his") && kept(&written, "l-hers"));
        written.doc.delete(&listed, 0_usize).unwrap();
        written.follow("an item is taken from a list");
        assert!(!kept(&written, "l-his"));

        let conversations = written
            .doc
            .put_object(automerge::ROOT, "conversations", ObjType::Map)
            .unwrap();
        let about = written
            .doc
            .put_object(&conversations, hers.as_str(), ObjType::Map)
            .unwrap();
        written.doc.put(&about, "title", "ours").unwrap();
        written.follow("a conversation is kept under its own id");
        written.doc.delete(&conversations, hers.as_str()).unwrap();
        written.follow("and taken away");

        written.doc.put(&messages, "m-open", "gone").unwrap();
        written.follow("a plain string takes a message's place");
        assert!(!kept(&written, "m-open"));
        let outer = written.child(&messages, "m-hers");
        written.doc.put(&outer, "said_in", hers.as_str()).unwrap();
        written.follow("an entry names a conversation beside its item's fields");
        let fresh = written
            .doc
            .put_object(&outer, ITEM_VALUE, ObjType::Map)
            .unwrap();
        written.doc.put(&fresh, "said_in", "general").unwrap();
        written.follow("an item's fields are put anew");
        written.doc.delete(&outer, "said_in").unwrap();
        written.follow("and the entry's own name is taken away");
        assert!(!kept(&written, "m-hers"));
        let body = written
            .doc
            .put_object(&fresh, "body", ObjType::Text)
            .unwrap();
        written.doc.splice_text(&body, 0, 0, "words").unwrap();
        written.follow("a text is put under a message");
        written.doc.splice_text(&body, 0, 2, "").unwrap();
        written.follow("and loses two letters");
    }
}
