//! The families text is set in on this device: what a reading face's picker offers.
//!
//! The host that runs the code answers it ([`FontHost`]): a window's host has the toolkit, whose
//! catalog holds the faces Tilo ships and the ones the machine installed. A host without the
//! toolkit answers none, and a view that offers a face's picker offers only the face it sets.

/// Where a host's text-setting families come from.
pub trait FontHost {
    /// Every family this device can set text in, each once.
    fn families(&self) -> Vec<String>;
}
