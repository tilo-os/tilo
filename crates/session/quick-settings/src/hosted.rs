//! Rows the desktop describes and the panel shows: the Devices section's
//! device fabric rows and the Clipboard section's copies.
//!
//! The desktop holds the device fabric and the clipboard history, so it
//! describes each row ([`HostedRow`]) while its section shows; the panel
//! lists the rows and names which of a row's buttons was pressed in its
//! effects ([`crate::RowPart`]).

use ui::icons::Icon;

/// What a row offers besides a press on its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostedActions {
    None,
    /// The row's one action, named on a button (stop, cancel, try again).
    Select(String),
    /// Two choices: allow and decline.
    Pair {
        first: (Icon, String),
        second: (Icon, String),
    },
    /// Driving another device with this machine's pointer. Where its
    /// display stands is arranged in its settings pane, which a press on
    /// the row opens.
    Control {
        name: String,
        /// This machine's pointer is driving it now.
        controlling: bool,
        /// It can be driven: online and not this machine.
        controllable: bool,
    },
    /// A copy's pin and remove.
    Clipboard {
        pinned: bool,
        pin: String,
        remove: String,
    },
}

/// One row's face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedRow {
    /// Names the row in the panel's effects: what the desktop knows it by.
    pub key: String,
    pub title: String,
    pub detail: String,
    pub icon: Icon,
    /// The row is the chosen one (the media source the volume follows).
    pub selected: bool,
    pub enabled: bool,
    pub actions: HostedActions,
}
