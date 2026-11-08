//! What pairs the panel's now playing with what another of the person's devices handed over.
//! The audio service owns the players and publishes them; what the other device offered is the
//! session's own, read off its fabric, and it is published beside the person's devices for the
//! panel to offer continuing it here.

use crate::Shared;

/// Land a session another of the person's devices hands this one, so the now-playing card offers to
/// continue it here; `None` clears it. The devices' reading carries it ([`crate::devices`]), so the
/// panel shows the offer as it shows the rows, and continuing it names the same one back.
pub(crate) fn handoff(shared: &Shared, offer: Option<tilo_quick_settings::MediaOffer>) {
    *shared.handoff.borrow_mut() = offer;
}
