//! What another of the person's devices has in front, as this device offers to continue it: the
//! bar's chip, and what pressing it or putting it away does here.

use tilo_resource::Effect;

/// What the offer's resource offers: what the person does with the chip.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.continuity")]
pub enum ContinuityAction {
    /// Open here what that device shows there
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Take {
        /// The offer, as the reading names it.
        #[param("The offer, as the reading names it")]
        offer: String,
    },
    /// Put the offer away here, until that device's front window moves again
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Dismiss {
        /// The offer, as the reading names it.
        #[param("The offer, as the reading names it")]
        offer: String,
    },
}

/// The most recent app another of the person's devices has in front, which this device has neither
/// taken nor put away: what the bar's chip draws and what taking it opens here. Every field is
/// empty when there is nothing to continue.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct ContinuityView {
    /// The offer, as taking it or putting it away names it.
    pub offer: String,
    /// The app's name, as the person at that device reads it.
    pub app: String,
    /// The app's id.
    pub app_id: String,
    /// That device, as the person named it.
    pub device: String,
    /// The resource its app shows, by its address.
    pub resource: String,
    /// The view it shows it through; empty for the app's primary one.
    pub view: String,
}

/// The offer as a view of another language reads it (`device.continuity`).
#[must_use]
pub(crate) fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<ContinuityView, ContinuityAction>(
        "continuity",
        "Continuity",
    )
}
