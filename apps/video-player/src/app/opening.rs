//! The handshake with the file chooser: what was asked, what came back, and what is still
//! opening. An answer to an older ask must never be taken for the current one.

use std::path::PathBuf;

use ui::framework::EffectReply;
use ui::portal::{ContentsChannel, PickedContents, PortalChannel};

/// One file-picker request at a time. A portal response cannot be cancelled, so the app
/// owns the receiver and drops it when a newer open supersedes it. `generation` names this
/// chooser, not the media it may select.
pub(super) struct PendingPortalOpen {
    pub(super) channel: PickerChannel,
    pub(super) generation: u64,
    pub(super) reply: Option<EffectReply>,
}

/// A host with paths answers with the chosen path; one without them, a browser page, hands
/// over the bytes.
pub(super) enum PickerChannel {
    Path(PortalChannel),
    Contents(ContentsChannel),
}

pub(super) enum Picked {
    Path(PathBuf),
    Contents(PickedContents),
}

pub(super) type PortalOpenOutcome = (u64, Result<Picked, Option<String>>);

/// An open awaiting its decoder's first picture: the picker can succeed while the decode
/// fails, stalls or is superseded.
pub(super) struct PendingMediaOpen {
    pub(super) reply: EffectReply,
    pub(super) source_id: String,
    pub(super) deadline: ui::time::Instant,
}
