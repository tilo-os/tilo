//! The picture this process's own person chose, as the identity service its launcher bound holds
//! it: lent to every face that shows them ([`tilo_ui_base::people_photos::install_lookup`]).

/// Lend the faces this process draws its own person's picture, read from the identity service
/// the launcher bound. A process's starter calls this once; the toolkit never does.
pub fn lend() {
    tilo_ui_base::people_photos::install_lookup(Some(|identity| {
        ::collaboration::local_identity::photo_for(identity).map(|photo| {
            tilo_ui_base::widget::avatar_face::profile_image(
                photo.rgba(),
                photo.width(),
                photo.height(),
            )
        })
    }));
}
