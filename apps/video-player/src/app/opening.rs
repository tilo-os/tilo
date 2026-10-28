//! The handshake with the file chooser: what was asked for, what came back, and
//! what is still being opened. A chooser answers in another process, so what
//! the player asked for has to be remembered until the answer arrives — and an
//! answer to an older ask must never be mistaken for the current one.

use std::path::PathBuf;

use ui::framework::EffectReply;
use ui::portal::{ContentsChannel, PickedContents, PortalChannel};

/// One file-picker request at a time.  A portal response is asynchronous and
/// cannot be cancelled at the protocol level, so the app owns the receiver and
/// drops it when a newer direct open supersedes the request.  `generation` is
/// deliberately separate from the media generation: it identifies *this
/// chooser* rather than the media that may eventually be selected.
pub(super) struct PendingPortalOpen {
    pub(super) channel: PickerChannel,
    pub(super) generation: u64,
    /// The caller of the open effect, answered once a picture decodes.
    pub(super) reply: Option<EffectReply>,
}

/// A host with paths answers with the chosen path; a host without them (a
/// browser page) hands over the file's bytes.
pub(super) enum PickerChannel {
    Path(PortalChannel),
    Contents(ContentsChannel),
}

/// What the picker chose, in the form the host could give it.
pub(super) enum Picked {
    Path(PathBuf),
    Contents(PickedContents),
}

/// A picker's answer, with the request generation it answers.
pub(super) type PortalOpenOutcome = (u64, Result<Picked, Option<String>>);

/// A canonical open invocation awaiting its decoder's first picture. This is
/// separate from the file-picker request: the picker can succeed while decode
/// fails, stalls, or is superseded before a frame appears.
pub(super) struct PendingMediaOpen {
    pub(super) reply: EffectReply,
    pub(super) source_id: String,
    pub(super) deadline: ui::time::Instant,
}
