//! A network several devices share, whose delivery the test owns:
//! [`FakeNet`], [`Peer`] and [`Converged`].
//!
//! A collaboration bug needs two devices, and a collaboration test that runs on
//! time — a socket, a sleep, a retry — reports a pass or a failure the machine's
//! load chose. This module makes the network a value a test steps through: a
//! device offers the messages it wrote when the test says so, and every device
//! takes the bytes that reached it in on the next step. Routing, drops and
//! merges therefore happen in one order on every run, on any machine.
//!
//! ```
//! use tilo_test_env::FakeNet;
//!
//! let mut net = FakeNet::new();
//! let alice = net.peer("alice");
//! let bob = net.peer("bob");
//! let mut seen: Vec<String> = Vec::new();
//!
//! net.publish(&alice, b"a sketch".to_vec());
//! assert!(seen.is_empty(), "nothing arrives before the test delivers");
//! net.deliver(|_, bytes| seen.push(String::from_utf8_lossy(bytes).into_owned()));
//! assert_eq!(seen, vec!["a sketch".to_owned()]);
//!
//! // Bob's laptop is on a train: what is published for him waits.
//! net.partition(&[bob.name()]);
//! net.publish(&alice, b"still writing".to_vec());
//! net.deliver(|_, bytes| seen.push(String::from_utf8_lossy(bytes).into_owned()));
//! assert_eq!(seen.len(), 1);
//! assert_eq!(net.held(), 1);
//!
//! net.heal(&[bob.name()]);
//! net.deliver(|_, bytes| seen.push(String::from_utf8_lossy(bytes).into_owned()));
//! assert_eq!(seen.len(), 2, "the net delivers what it held");
//! ```
//!
//! [`FakeNet`] itself moves opaque bytes: a notebook's changes, a room's
//! presence frames, an app's delta. [`document`] is the Automerge pairing on top
//! of it — what to publish from a replica, and what it means for two devices to
//! have converged — since that is the shape a collaboration scenario drives.
//!
//! A partition holds what was published while a device was cut instead of
//! dropping it, so a healed device catches up rather than having to rebuild
//! from the others' history. [`FakeNet::forget_held`] is the other network: one
//! whose lost frames are lost, which is the case anti-entropy has to recover
//! from.

#[cfg(feature = "document")]
pub mod document;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A network several devices share. See the module docs.
#[derive(Debug, Default)]
pub struct FakeNet {
    peers: BTreeMap<String, PeerRecord>,
    /// Devices off the network: what they publish reaches nobody, and what the
    /// others publish is held for them rather than dropped.
    cut: BTreeSet<String>,
    /// What was published while a device was cut, kept for it. Cleared by
    /// [`Self::forget_held`].
    held: Vec<(String, Vec<u8>)>,
    /// Everything offered to the open net, in the order it happened: the
    /// traffic a test asserts on.
    log: Vec<(String, Vec<u8>)>,
    deliveries: u64,
}

#[derive(Debug, Default)]
struct PeerRecord {
    /// The changes this device has offered the net.
    published: Vec<Vec<u8>>,
    /// What reached this device that it has not taken in yet: what the next
    /// [`FakeNet::deliver`] hands it.
    inbox: Vec<(String, Vec<u8>)>,
}

impl FakeNet {
    /// A net with no device on it: everybody connected.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A device on this net, under `name`. Asking for a name the net already
    /// knows answers a handle on the same device, keeping what it published:
    /// two windows of one device, or an app closed and opened again.
    pub fn peer(&mut self, name: &str) -> Peer {
        self.peers.entry(name.to_owned()).or_default();
        Peer {
            name: name.to_owned(),
        }
    }

    /// The names of the devices on the net, in order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.peers.keys().map(String::as_str).collect()
    }

    /// Cut `devices` off from the net: what the others offer from now on does
    /// not reach them, and what they offer reaches nobody.
    ///
    /// What the net held for them arrives once it heals, as a device that
    /// reconnects catches up on what it missed.
    pub fn partition(&mut self, devices: &[&str]) {
        for name in devices {
            self.cut.insert((*name).to_owned());
        }
    }

    /// Let `devices` back onto the net: the next [`Self::deliver`] hands them
    /// what the net held for them.
    pub fn heal(&mut self, devices: &[&str]) {
        for name in devices {
            if !self.cut.remove(*name) {
                continue;
            }
            for (author, bytes) in self.held.clone() {
                if author == *name {
                    continue;
                }
                if let Some(record) = self.peers.get_mut(*name) {
                    record.inbox.push((author, bytes));
                }
            }
        }
    }

    /// Lose what the net held for cut devices, rather than delivering it when
    /// they heal: a network whose frames are dropped, where a device that comes
    /// back has to be brought up to date from the others' history alone.
    pub fn forget_held(&mut self) {
        self.held.clear();
    }

    /// Whether `device` is on the net now.
    #[must_use]
    pub fn is_connected(&self, device: &str) -> bool {
        self.peers.contains_key(device) && !self.cut.contains(device)
    }

    /// Offer `bytes` — what a device wrote — to the net, as that device.
    ///
    /// Offering nothing offers nothing: a caller that hands over what the net
    /// already carries sends no second message, which
    /// [`document::FakeDocumentNet::publish`] does for a whole replica.
    pub fn publish(&mut self, device: &Peer, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let Some(record) = self.peers.get_mut(&device.name) else {
            return;
        };
        record.published.push(bytes.clone());
        if self.cut.contains(&device.name) {
            return;
        }
        self.log.push((device.name.clone(), bytes.clone()));
        let open: Vec<String> = self
            .peers
            .keys()
            .filter(|name| name.as_str() != device.name && !self.cut.contains(*name))
            .cloned()
            .collect();
        for name in open {
            if let Some(record) = self.peers.get_mut(&name) {
                record.inbox.push((device.name.clone(), bytes.clone()));
            }
        }
        if !self.cut.is_empty() {
            self.held.push((device.name.clone(), bytes));
        }
    }

    /// The traffic that reached the open net, in the order it was published:
    /// `(author, bytes)`.
    #[must_use]
    pub fn log(&self) -> &[(String, Vec<u8>)] {
        &self.log
    }

    /// How many messages have been handed to a device so far.
    #[must_use]
    pub fn deliveries(&self) -> u64 {
        self.deliveries
    }

    /// How many messages are waiting for a cut device, which a heal delivers.
    #[must_use]
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Hand `device` what reached it to `apply`, as it receives it, and forget
    /// the message: a device takes each one in once.
    ///
    /// `apply` is `(author, bytes)`. A message the receiver drops is still
    /// spent, as a frame a device ignored is not offered again. A cut device
    /// receives nothing, and keeps what waits for it.
    pub fn deliver_to(&mut self, device: &str, mut apply: impl FnMut(&str, &[u8])) {
        if self.cut.contains(device) {
            return;
        }
        let Some(record) = self.peers.get_mut(device) else {
            return;
        };
        for (author, bytes) in std::mem::take(&mut record.inbox) {
            apply(&author, &bytes);
            self.deliveries = self.deliveries.saturating_add(1);
        }
    }

    /// Hand every connected device what reached it, in name order, as
    /// [`Self::deliver_to`] does for one.
    pub fn deliver(&mut self, mut apply: impl FnMut(&str, &[u8])) {
        let names: Vec<String> = self.peers.keys().cloned().collect();
        for name in names {
            self.deliver_to(&name, &mut apply);
        }
    }

    /// What the net held for cut devices, as `(author, bytes)`.
    #[must_use]
    pub fn held_traffic(&self) -> Vec<(String, Vec<u8>)> {
        self.held.clone()
    }

    /// Everything `name` offered the net.
    #[must_use]
    pub fn published_by(&self, name: &str) -> Vec<Vec<u8>> {
        self.peers
            .get(name)
            .map(|record| record.published.clone())
            .unwrap_or_default()
    }
}

/// One device on a [`FakeNet`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Peer {
    name: String,
}

impl Peer {
    /// The device's name on the net.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// What two devices disagree on, device by device and path by path.
///
/// A `Converged` is what an assertion reports: [`Self::holds`] says whether the
/// devices agree, [`Self::disagreements`] says what does not, and
/// [`Self::assert_holds`] fails the calling test with each line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Converged {
    disagreements: Vec<String>,
}

impl From<Vec<String>> for Converged {
    fn from(disagreements: Vec<String>) -> Self {
        Self { disagreements }
    }
}

impl Converged {
    /// Whether there is nothing to disagree on.
    #[must_use]
    pub fn holds(&self) -> bool {
        self.disagreements.is_empty()
    }

    /// What disagrees, one line per device and path.
    #[must_use]
    pub fn disagreements(&self) -> &[String] {
        &self.disagreements
    }

    /// Fail the calling test unless the devices agree, naming what does not.
    ///
    /// # Panics
    ///
    /// Panics when a device holds something the other does not, or the two
    /// differ at one of the paths the test named.
    #[track_caller]
    pub fn assert_holds(&self) {
        assert!(
            self.holds(),
            "the devices did not converge: {}",
            self.disagreements.join("; "),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two devices on one net.
    fn two_devices() -> (FakeNet, Peer, Peer) {
        let mut net = FakeNet::new();
        let alice = net.peer("alice");
        let bob = net.peer("bob");
        (net, alice, bob)
    }

    #[test]
    fn a_published_message_reaches_every_connected_device_once_the_net_delivers() {
        let (mut net, alice, bob) = two_devices();
        net.publish(&alice, b"first".to_vec());
        assert_eq!(net.deliveries(), 0, "nothing arrives before delivery");
        let mut seen: Vec<(String, Vec<u8>)> = Vec::new();
        net.deliver(|author, bytes| seen.push((author.to_owned(), bytes.to_vec())));
        assert_eq!(seen, vec![("alice".to_owned(), b"first".to_vec())]);
        assert_eq!(net.deliveries(), 1);
        assert_eq!(net.log(), &[("alice".to_owned(), b"first".to_vec())]);
        assert_eq!(net.names(), vec!["alice", "bob"]);
        assert!(net.is_connected(bob.name()));
    }

    #[test]
    fn nothing_is_offered_when_there_is_nothing_to_offer() {
        let (mut net, alice, _bob) = two_devices();
        net.publish(&alice, Vec::new());
        assert!(net.log().is_empty());
        assert_eq!(net.deliveries(), 0);
    }

    #[test]
    fn a_partition_holds_the_traffic_and_a_heal_delivers_it() {
        let (mut net, alice, bob) = two_devices();
        net.partition(&[bob.name()]);
        assert!(!net.is_connected(bob.name()));
        net.publish(&alice, b"while bob was away".to_vec());
        let mut seen: Vec<Vec<u8>> = Vec::new();
        net.deliver(|_, bytes| seen.push(bytes.to_vec()));
        assert!(seen.is_empty(), "a cut device receives nothing");
        assert_eq!(net.held(), 1, "what was published for bob is held");

        net.heal(&[bob.name()]);
        assert!(net.is_connected(bob.name()));
        net.deliver(|_, bytes| seen.push(bytes.to_vec()));
        assert_eq!(seen, vec![b"while bob was away".to_vec()]);
    }

    #[test]
    fn a_cut_device_sends_nothing_until_it_heals() {
        let (mut net, alice, bob) = two_devices();
        net.partition(&[bob.name()]);
        assert!(!net.is_connected(bob.name()));
        net.publish(&bob, b"written on the train".to_vec());
        assert!(net.log().is_empty(), "nothing leaves a device off the net");
        let mut seen: Vec<Vec<u8>> = Vec::new();
        net.deliver(|_, bytes| seen.push(bytes.to_vec()));
        assert!(seen.is_empty());

        net.heal(&["bob"]);
        net.publish(&bob, b"written on the train".to_vec());
        assert_eq!(net.log().len(), 1);
        net.deliver(|_, bytes| seen.push(bytes.to_vec()));
        assert_eq!(seen, vec![b"written on the train".to_vec()]);
        assert_eq!(net.published_by(alice.name()), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn traffic_held_for_a_cut_device_can_be_forgotten() {
        let (mut net, alice, _bob) = two_devices();
        net.partition(&["bob"]);
        net.publish(&alice, b"lost".to_vec());
        net.forget_held();
        net.heal(&["bob"]);
        let mut seen: Vec<Vec<u8>> = Vec::new();
        net.deliver(|_, bytes| seen.push(bytes.to_vec()));
        assert!(seen.is_empty(), "the frames were lost");
    }

    #[test]
    fn the_same_steps_deliver_the_same_messages_in_the_same_order() {
        let run = || {
            let (mut net, alice, bob) = two_devices();
            let carol = net.peer("carol");
            net.publish(&alice, b"one".to_vec());
            net.partition(&["bob"]);
            net.publish(&bob, b"two".to_vec());
            net.publish(&carol, b"three".to_vec());
            let mut seen: Vec<(String, String)> = Vec::new();
            net.deliver(|author, bytes| {
                seen.push((
                    author.to_owned(),
                    String::from_utf8_lossy(bytes).into_owned(),
                ));
            });
            net.heal(&["bob"]);
            net.publish(&alice, b"four".to_vec());
            net.deliver(|author, bytes| {
                seen.push((
                    author.to_owned(),
                    String::from_utf8_lossy(bytes).into_owned(),
                ));
            });
            let names: Vec<String> = net.names().into_iter().map(str::to_owned).collect();
            (seen, net.deliveries(), names)
        };
        let first = run();
        for _ in 0..100 {
            assert_eq!(run(), first, "the same steps deliver the same traffic");
        }
    }
}
