//! Who may send a call's frames.

use std::collections::BTreeSet;

/// The devices a workspace's roster names: the only ones whose frames this
/// device opens.
///
/// A device removed from the channel keeps every key it was given, so a
/// frame it goes on sealing under an old epoch still authenticates; the
/// roster is what refuses it. Who is on the roster is the workspace's, not
/// this crate's: a carrier builds the roster from the devices the workspace
/// certifies, and builds a new one when that changes rather than keeping a
/// removed device's frames openable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roster {
    devices: BTreeSet<[u8; 32]>,
}

impl Roster {
    /// A roster naming `devices` (each a device's public key).
    pub fn new(devices: impl IntoIterator<Item = [u8; 32]>) -> Self {
        Self {
            devices: devices.into_iter().collect(),
        }
    }

    /// Whether `device` may send.
    pub fn admits(&self, device: [u8; 32]) -> bool {
        self.devices.contains(&device)
    }

    /// The devices it names, in key order.
    pub fn devices(&self) -> impl Iterator<Item = [u8; 32]> + '_ {
        self.devices.iter().copied()
    }

    /// How many devices it names.
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Whether it names no device, and so opens nothing.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::Roster;

    #[test]
    fn a_roster_admits_exactly_the_devices_it_names() {
        let roster = Roster::new([[1; 32], [2; 32], [1; 32]]);
        assert!(roster.admits([1; 32]));
        assert!(roster.admits([2; 32]));
        assert!(!roster.admits([3; 32]));
        assert_eq!(roster.len(), 2);
        assert_eq!(roster.devices().count(), 2);
        assert!(Roster::default().is_empty());
        assert!(!Roster::new([[1; 32]]).is_empty());
    }
}
