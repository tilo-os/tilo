//! Unlocking an encrypted root in early boot with the consent of another of
//! the owner's devices (docs/device-reach.md, "Unlocking it after a reboot").
//!
//! The volume's sibling key slot opens with a passphrase derived from a
//! device half kept on this machine and a sibling half kept only by the
//! owner's other devices. Early boot asks them for their half over the peer
//! network; one releases it for this boot alone after the owner approves.
//!
//! * [`token`]: what the volume's LUKS2 header carries for it.
//! * [`material`]: the device half, the boot key and the unlock networks.
//! * [`seal`]: sealing the material to the TPM where there is one.
//! * [`unlock`]: one early-boot attempt; [`askpass`] answers the prompt with
//!   the result and [`network`] brings up the links to get it.
//! * [`exchange`]: the exchange with a sibling over the peer network.
//! * [`enrol`]: adding the factor to a volume and registering it.
//! * [`luks`]: the volume's slots and token, through cryptsetup.

pub mod askpass;
pub mod enrol;
pub mod exchange;
pub mod exec;
pub mod luks;
pub mod material;
pub mod network;
pub mod seal;
pub mod token;
pub mod unlock;
