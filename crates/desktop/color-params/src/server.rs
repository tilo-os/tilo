//! The protocol's server objects: what a `wp_color_manager_v1` advertises, and the set requests
//! of a parametric creator.

use wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::{
    Feature, Primaries, RenderIntent, TransferFunction, WpColorManagerV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_params_v1::Request;

use crate::{Set, Support};

/// Tell a client's newly bound manager what `support` holds, then `done`.
pub fn advertise(manager: &WpColorManagerV1, support: &Support) {
    for intent in &support.intents {
        if let Ok(intent) = RenderIntent::try_from(*intent) {
            manager.supported_intent(intent);
        }
    }
    for feature in &support.features {
        if let Ok(feature) = Feature::try_from(*feature) {
            manager.supported_feature(feature);
        }
    }
    for tf in &support.transfers {
        if let Ok(tf) = TransferFunction::try_from(*tf) {
            manager.supported_tf_named(tf);
        }
    }
    for primaries in &support.primaries {
        if let Ok(primaries) = Primaries::try_from(*primaries) {
            manager.supported_primaries_named(primaries);
        }
    }
    manager.done();
}

/// The parameter a creator's request sets; any other request (`create`) back as it came.
pub fn set_of(request: Request) -> Result<Set, Request> {
    Ok(match request {
        Request::SetTfNamed { tf } => Set::TfNamed(tf.into()),
        Request::SetTfPower { eexp } => Set::TfPower(eexp),
        Request::SetPrimariesNamed { primaries } => Set::PrimariesNamed(primaries.into()),
        Request::SetPrimaries {
            r_x,
            r_y,
            g_x,
            g_y,
            b_x,
            b_y,
            w_x,
            w_y,
        } => Set::Primaries([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]),
        Request::SetLuminances {
            min_lum,
            max_lum,
            reference_lum,
        } => Set::Luminances(min_lum, max_lum, reference_lum),
        Request::SetMasteringDisplayPrimaries {
            r_x,
            r_y,
            g_x,
            g_y,
            b_x,
            b_y,
            w_x,
            w_y,
        } => Set::MasteringPrimaries([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]),
        Request::SetMasteringLuminance { min_lum, max_lum } => {
            Set::MasteringLuminance(min_lum, max_lum)
        }
        Request::SetMaxCll { max_cll } => Set::MaxCll(max_cll),
        Request::SetMaxFall { max_fall } => Set::MaxFall(max_fall),
        other => return Err(other),
    })
}
