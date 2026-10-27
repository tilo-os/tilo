//! What a [`super::FakeNet`] carries between Automerge devices: publish a
//! replica's changes, and check that the devices converged.
//!
//! The net itself moves bytes. This module is the pairing a scenario on a CRDT
//! usually wants: [`FakeDocumentNet::publish`] sends the changes a document
//! holds that the net does not carry yet — so offering a replica again after a
//! heal sends only what was written meanwhile — and
//! [`FakeDocumentNet::converged`] reports which device or path disagrees.
//!
//! ```
//! use automerge::transaction::Transactable as _;
//! use automerge::{AutoCommit, ROOT};
//! use tilo_test_env::FakeDocumentNet;
//!
//! let mut net = FakeDocumentNet::new();
//! let alice = net.peer("alice");
//! let bob = net.peer("bob");
//! let mut a = AutoCommit::new();
//! let mut b = AutoCommit::new();
//!
//! a.put(ROOT, "title", "Shared").unwrap();
//! net.publish(&alice, &mut a);
//! net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
//! net.converged(&alice, &mut a, &bob, &mut b, &["title"])
//!     .assert_holds();
//! ```

use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, ReadDoc as _, ScalarValue, Value};
use tilo_automerge::ApplyIncoming as _;

use super::{Converged, FakeNet, Peer};

/// A [`FakeNet`] whose devices carry Automerge changes.
///
/// A `FakeDocumentNet` is a `FakeNet` with the CRDT pairing on top of it: the
/// same peers, partitions, heals and delivery order, plus the two things a
/// document test asks of them.
#[derive(Debug, Default)]
pub struct FakeDocumentNet {
    net: FakeNet,
    offered: std::collections::BTreeMap<String, Vec<ChangeHash>>,
}

impl FakeDocumentNet {
    /// A net with no device on it: everybody connected.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A device on this net, under `name`. See [`FakeNet::peer`].
    pub fn peer(&mut self, name: &str) -> Peer {
        self.net.peer(name)
    }

    /// The net itself, for partitions, heals and the delivery log.
    pub fn net(&mut self) -> &mut FakeNet {
        &mut self.net
    }

    /// Lose what the net held for cut devices, as [`FakeNet::forget_held`]
    /// does, together with the record of what was published for them: a frame
    /// that was dropped has to be offered again, from the replica that wrote it.
    pub fn forget_held(&mut self) {
        for (author, bytes) in self.net.held_traffic() {
            let Some(offered) = self.offered.get_mut(&author) else {
                continue;
            };
            let mut carried = AutoCommit::new();
            if carried.load_incoming(&bytes).is_err() {
                continue;
            }
            let gone = written_hashes(&mut carried);
            offered.retain(|hash| !gone.contains(hash));
        }
        self.net.forget_held();
    }

    /// The names of the devices on the net, in order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.net.names()
    }

    /// Offer the changes `document` holds that the net does not carry yet, as
    /// `device` publishes them.
    ///
    /// Offering a replica again sends only what is new: the bytes already
    /// offered are compared change by change (by hash), so a device that
    /// published a change before a partition, or in an earlier step, does not
    /// publish it a second time. A device therefore catches up after a heal by
    /// offering the same replica again.
    pub fn publish(&mut self, device: &Peer, document: &mut AutoCommit) {
        let carried = self.offered.entry(device.name().to_owned()).or_default();
        let novel = unwritten(document, carried);
        if novel.is_empty() {
            return;
        }
        let delta = document.save_after(carried);
        let reached = self.net.log().len();
        self.net.publish(device, delta);
        if self.net.log().len() > reached {
            carried.extend(novel);
        }
    }

    /// Hand every device what reached it, taking it into that device's
    /// replica: `(peer, document)` pairs, one per device.
    ///
    /// A message is taken in by loading it as a replica and merging that in,
    /// which is how the app path applies a peer's delta.
    pub fn deliver_into(&mut self, devices: &mut [(&Peer, &mut AutoCommit)]) {
        for (peer, document) in devices.iter_mut() {
            let (peer, document) = (peer.clone(), &mut **document);
            self.net.deliver_to(peer.name(), |_, bytes| {
                // A frame that will not load is a sender that got it wrong; the
                // convergence check names the device that is missing it.
                let _ = document.load_incoming(bytes);
            });
        }
    }

    /// What `document` has not taken in of what `author` published.
    ///
    /// An author that published nothing has nothing to take in, which is how
    /// two devices that never wrote agree.
    fn missing(&self, document: &mut AutoCommit, author: &Peer) -> Vec<ChangeHash> {
        let Some(offered) = self.offered.get(author.name()) else {
            return Vec::new();
        };
        let taken = written_hashes(document);
        offered
            .iter()
            .copied()
            .filter(|hash| !taken.contains(hash))
            .collect()
    }

    /// Whether `first` and `second` agree: each holds every change the other
    /// published, and both hold the same value at each of `paths` (a path is
    /// read by [`at_path`]).
    #[must_use]
    pub fn converged(
        &self,
        first: &Peer,
        first_doc: &mut AutoCommit,
        second: &Peer,
        second_doc: &mut AutoCommit,
        paths: &[&str],
    ) -> Converged {
        let mut disagreements = Vec::new();
        let first_missing = self.missing(second_doc, first);
        if !first_missing.is_empty() {
            disagreements.push(format!(
                "{} has not taken in what {} published",
                second.name(),
                first.name(),
            ));
        }
        let second_missing = self.missing(first_doc, second);
        if !second_missing.is_empty() {
            disagreements.push(format!(
                "{} has not taken in what {} published",
                first.name(),
                second.name(),
            ));
        }
        for path in paths {
            let mine = at_path(first_doc, path);
            let theirs = at_path(second_doc, path);
            if mine != theirs {
                disagreements.push(format!(
                    "{} has {path} = {mine:?}, {} has {theirs:?}",
                    first.name(),
                    second.name(),
                ));
            }
        }
        Converged::from(disagreements)
    }

    /// Fail the calling test unless every device agrees with every other, at
    /// each of `paths`: the assertion a scenario makes once the net delivered.
    ///
    /// # Panics
    ///
    /// Panics naming the device and the path that disagree.
    #[track_caller]
    pub fn assert_converged(&mut self, devices: &mut [(&Peer, &mut AutoCommit)], paths: &[&str]) {
        let mut disagreements: Vec<String> = Vec::new();
        let names: Vec<Peer> = devices.iter().map(|(peer, _)| (*peer).clone()).collect();
        for (peer, document) in devices.iter_mut() {
            for author in &names {
                if author.name() == peer.name() {
                    continue;
                }
                if !self.missing(document, author).is_empty() {
                    disagreements.push(format!("{peer} has not taken in what {author} published"));
                }
            }
        }
        for path in paths {
            let mut held: Vec<(String, Option<serde_json::Value>)> = Vec::new();
            for (peer, document) in devices.iter_mut() {
                held.push((peer.to_string(), at_path(document, path)));
            }
            let Some((first, value)) = held.first() else {
                break;
            };
            if held.iter().all(|(_, other)| other == value) {
                continue;
            }
            let others: Vec<String> = held
                .iter()
                .skip(1)
                .map(|(peer, other)| format!("{peer} has {other:?}"))
                .collect();
            disagreements.push(format!(
                "{path} disagrees: {first} has {value:?}, {}",
                others.join(", "),
            ));
        }
        assert!(
            disagreements.is_empty(),
            "the devices did not converge: {}",
            disagreements.join("; "),
        );
    }
}

/// The hashes of the changes `document` holds that `written` does not name.
///
/// A change is compared by hash rather than looked up through the document, so
/// a device that has taken in the first change of a run and not the second is
/// missing exactly the second, whichever actor wrote them.
fn unwritten(document: &mut AutoCommit, written: &[ChangeHash]) -> Vec<ChangeHash> {
    let held = written_hashes(document);
    held.into_iter()
        .filter(|hash| !written.contains(hash))
        .collect()
}

/// The hashes of every change `document` holds.
fn written_hashes(document: &mut AutoCommit) -> Vec<ChangeHash> {
    document
        .get_changes(&[])
        .into_iter()
        .map(|change| change.hash())
        .collect()
}

/// The value at `path` in `document`, as a person sees it: a map key or a list
/// index per segment, separated by dots, where an index is written `[n]` after
/// the key that holds the list — `title`, `pages[0].title`.
///
/// A text object reads as its text, and a list or a map as `Null`: a scenario
/// asserts on the value a person sees, and names the field inside a map when it
/// wants that. A path the document does not hold reads as `None`.
#[must_use]
pub fn at_path(document: &mut AutoCommit, path: &str) -> Option<serde_json::Value> {
    let mut object = automerge::ROOT;
    for segment in path.split('.') {
        let (key, index) = match segment.split_once('[') {
            Some((key, rest)) => (key, Some(rest.strip_suffix(']')?.parse::<usize>().ok()?)),
            None => (segment, None),
        };
        let (value, id) = document.get(&object, key).ok().flatten()?;
        let (value, id) = match (value, index) {
            (Value::Object(ObjType::List), Some(index)) => {
                document.get(&id, index).ok().flatten()?
            }
            (value, Some(_)) => return read(document, value, &id),
            (value, None) => (value, id),
        };
        match value {
            Value::Object(ObjType::Map | ObjType::Table | ObjType::List) => object = id,
            value => return read(document, value, &id),
        }
    }
    Some(serde_json::Value::Null)
}

/// A value the path ended on, as a person sees it.
fn read(document: &AutoCommit, value: Value<'_>, id: &ObjId) -> Option<serde_json::Value> {
    match value {
        Value::Object(ObjType::Text) => Some(serde_json::Value::String(document.text(id).ok()?)),
        Value::Object(_) => Some(serde_json::Value::Null),
        Value::Scalar(scalar) => Some(match scalar.into_owned() {
            ScalarValue::Str(text) => serde_json::Value::String(text.to_string()),
            ScalarValue::Int(number) => serde_json::Value::from(number),
            ScalarValue::Uint(number) => serde_json::Value::from(number),
            ScalarValue::F64(number) => serde_json::Value::from(number),
            ScalarValue::Counter(counter) => serde_json::Value::from(counter.to_string()),
            ScalarValue::Timestamp(stamp) => serde_json::Value::from(stamp),
            ScalarValue::Boolean(flag) => serde_json::Value::Bool(flag),
            ScalarValue::Bytes(bytes) => serde_json::Value::from(bytes),
            ScalarValue::Unknown { .. } | ScalarValue::Null => serde_json::Value::Null,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;

    /// Two devices on one net, each holding its own replica.
    fn two_devices() -> (FakeDocumentNet, Peer, Peer, AutoCommit, AutoCommit) {
        let mut net = FakeDocumentNet::new();
        let alice = net.peer("alice");
        let bob = net.peer("bob");
        let mut a = AutoCommit::new();
        a.set_actor(automerge::ActorId::from(b"alice".as_slice()));
        let mut b = AutoCommit::new();
        b.set_actor(automerge::ActorId::from(b"bob".as_slice()));
        (net, alice, bob, a, b)
    }

    #[test]
    fn a_published_edit_reaches_the_other_device_once_the_net_delivers() {
        let (mut net, alice, bob, mut a, mut b) = two_devices();
        a.put(automerge::ROOT, "title", "Shared").unwrap();
        net.publish(&alice, &mut a);
        assert_eq!(
            at_path(&mut b, "title"),
            None,
            "nothing reaches a device before delivery",
        );
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
        assert_eq!(
            at_path(&mut b, "title"),
            Some(serde_json::Value::String("Shared".into())),
        );
        net.converged(&alice, &mut a, &bob, &mut b, &["title"])
            .assert_holds();
        assert_eq!(net.net().log().len(), 1, "one publish, one message");
    }

    #[test]
    fn publishing_a_replica_again_sends_only_what_is_new() {
        let (mut net, alice, _bob, mut a, _b) = two_devices();
        a.put(automerge::ROOT, "title", "First").unwrap();
        net.publish(&alice, &mut a);
        net.publish(&alice, &mut a);
        assert_eq!(net.net().log().len(), 1, "the same change is offered once");
        a.put(automerge::ROOT, "body", "Second").unwrap();
        net.publish(&alice, &mut a);
        assert_eq!(net.net().log().len(), 2, "a new change is offered");
        net.publish(&alice, &mut a);
        assert_eq!(net.net().log().len(), 2, "and only once");
    }

    #[test]
    fn a_partition_holds_the_traffic_and_a_heal_delivers_it() {
        let (mut net, alice, bob, mut a, mut b) = two_devices();
        a.put(automerge::ROOT, "title", "before").unwrap();
        net.publish(&alice, &mut a);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);

        net.net().partition(&["bob"]);
        a.put(automerge::ROOT, "title", "while bob was away")
            .unwrap();
        net.publish(&alice, &mut a);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
        assert_eq!(
            at_path(&mut b, "title"),
            Some(serde_json::Value::String("before".into())),
            "a cut device receives nothing",
        );
        assert!(!net.converged(&alice, &mut a, &bob, &mut b, &[]).holds());
        assert_eq!(net.net().held(), 1, "what was published for bob is held");

        net.net().heal(&["bob"]);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
        assert_eq!(
            at_path(&mut b, "title"),
            Some(serde_json::Value::String("while bob was away".into())),
        );
        net.converged(&alice, &mut a, &bob, &mut b, &["title"])
            .assert_holds();
    }

    #[test]
    fn a_cut_device_catches_up_when_the_net_heals() {
        let (mut net, alice, bob, mut a, mut b) = two_devices();
        net.net().partition(&["bob"]);
        b.put(automerge::ROOT, "body", "written on the train")
            .unwrap();
        net.publish(&bob, &mut b);
        assert!(
            net.net().log().is_empty(),
            "nothing leaves a device off the net",
        );
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
        assert_eq!(at_path(&mut a, "body"), None);

        net.net().heal(&["bob"]);
        net.publish(&bob, &mut b);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
        assert_eq!(
            at_path(&mut a, "body"),
            Some(serde_json::Value::String("written on the train".into())),
        );
        net.converged(&alice, &mut a, &bob, &mut b, &[])
            .assert_holds();
    }

    #[test]
    fn two_sides_of_a_partition_merge_when_the_net_heals() {
        let (mut net, alice, bob, mut a, mut b) = two_devices();
        a.put(automerge::ROOT, "title", "shared").unwrap();
        net.publish(&alice, &mut a);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);

        net.net().partition(&["bob"]);
        a.put(automerge::ROOT, "title", "from alice").unwrap();
        b.put(automerge::ROOT, "body", "from bob").unwrap();
        net.publish(&alice, &mut a);
        net.publish(&bob, &mut b);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
        assert!(
            !net.converged(&alice, &mut a, &bob, &mut b, &["title"])
                .holds(),
            "a partition keeps them apart",
        );

        net.net().heal(&["bob"]);
        net.publish(&alice, &mut a);
        net.publish(&bob, &mut b);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
        net.converged(&alice, &mut a, &bob, &mut b, &["title", "body"])
            .assert_holds();
        assert_eq!(
            at_path(&mut a, "title"),
            Some(serde_json::Value::String("from alice".into())),
            "the later write wins, whichever side of the partition it was made on",
        );
        assert_eq!(
            at_path(&mut b, "body"),
            Some(serde_json::Value::String("from bob".into())),
        );
    }

    #[test]
    fn traffic_held_for_a_cut_device_can_be_forgotten_and_offered_again() {
        let (mut net, alice, bob, mut a, mut b) = two_devices();
        net.net().partition(&["bob"]);
        a.put(automerge::ROOT, "title", "lost").unwrap();
        net.publish(&alice, &mut a);
        net.forget_held();
        net.net().heal(&["bob"]);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
        assert_eq!(at_path(&mut b, "title"), None, "the frames were lost");

        net.publish(&alice, &mut a);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
        assert_eq!(
            at_path(&mut b, "title"),
            Some(serde_json::Value::String("lost".into())),
            "offering the replica again offers what was lost",
        );
        net.converged(&alice, &mut a, &bob, &mut b, &["title"])
            .assert_holds();
    }

    #[test]
    fn the_same_steps_deliver_the_same_traffic_in_the_same_order() {
        let run = || {
            let (mut net, alice, bob, mut a, mut b) = two_devices();
            a.put(automerge::ROOT, "title", "one").unwrap();
            net.publish(&alice, &mut a);
            net.net().partition(&["bob"]);
            b.put(automerge::ROOT, "body", "two").unwrap();
            net.publish(&bob, &mut b);
            net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
            net.net().heal(&["bob"]);
            net.publish(&alice, &mut a);
            net.publish(&bob, &mut b);
            net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
            net.converged(&alice, &mut a, &bob, &mut b, &["title", "body"])
                .assert_holds();
            let authors: Vec<String> = net
                .net()
                .log()
                .iter()
                .map(|(author, _)| author.clone())
                .collect();
            (authors, net.net().deliveries())
        };
        let first = run();
        for _ in 0..100 {
            assert_eq!(run(), first, "the same steps deliver the same traffic");
        }
    }

    #[test]
    fn a_converged_assertion_names_the_path_that_disagrees() {
        let (mut net, alice, bob, mut a, mut b) = two_devices();
        a.put(automerge::ROOT, "title", "alice's").unwrap();
        net.publish(&alice, &mut a);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b)]);
        net.converged(&alice, &mut a, &bob, &mut b, &["title", "body"])
            .assert_holds();

        b.put(automerge::ROOT, "body", "bob's").unwrap();
        let converged = net.converged(&alice, &mut a, &bob, &mut b, &["body"]);
        assert!(!converged.holds());
        assert!(
            converged
                .disagreements()
                .iter()
                .any(|line| line.contains("alice has body = None")
                    && line.contains("bob has Some(String(\"bob's\"))")),
            "the report names the path, both devices and their values: {:?}",
            converged.disagreements(),
        );
    }

    #[test]
    fn a_path_reads_through_maps_and_lists_to_the_value_a_person_sees() {
        let mut document = AutoCommit::new();
        document.put(automerge::ROOT, "title", "Notebook").unwrap();
        document.put(automerge::ROOT, "count", 3_i64).unwrap();
        document.put(automerge::ROOT, "pinned", true).unwrap();
        let pages = document
            .put_object(automerge::ROOT, "pages", ObjType::List)
            .unwrap();
        let page = document.insert_object(&pages, 0, ObjType::Map).unwrap();
        document.put(&page, "title", "First").unwrap();
        let text = document.put_object(&page, "text", ObjType::Text).unwrap();
        document.splice_text(&text, 0, 0, "hello").unwrap();

        assert_eq!(
            at_path(&mut document, "title"),
            Some(serde_json::Value::String("Notebook".into())),
        );
        assert_eq!(at_path(&mut document, "count"), Some(3.into()));
        assert_eq!(at_path(&mut document, "pinned"), Some(true.into()));
        assert_eq!(
            at_path(&mut document, "pages[0].title"),
            Some(serde_json::Value::String("First".into())),
        );
        assert_eq!(
            at_path(&mut document, "pages[0].text"),
            Some(serde_json::Value::String("hello".into())),
        );
        assert_eq!(at_path(&mut document, "pages[1].title"), None);
        assert_eq!(at_path(&mut document, "missing"), None);
    }

    #[test]
    fn a_net_carries_every_device_it_is_given() {
        let (mut net, alice, bob, mut a, mut b) = two_devices();
        let carol = net.peer("carol");
        let mut c = AutoCommit::new();
        c.set_actor(automerge::ActorId::from(b"carol".as_slice()));
        a.put(automerge::ROOT, "title", "from alice").unwrap();
        c.put(automerge::ROOT, "body", "from carol").unwrap();
        net.publish(&alice, &mut a);
        net.publish(&carol, &mut c);
        net.deliver_into(&mut [(&alice, &mut a), (&bob, &mut b), (&carol, &mut c)]);
        net.assert_converged(
            &mut [(&alice, &mut a), (&bob, &mut b), (&carol, &mut c)],
            &["title", "body"],
        );
        assert_eq!(net.names(), vec!["alice", "bob", "carol"]);
    }
}
