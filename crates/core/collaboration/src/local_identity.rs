//! Who sits at this device: the process's one identity boundary
//! (`tilo_local_identity`), the native source that reads the workspace identity
//! service a launcher bound, and the colour the person chose, which their
//! settings keep.

pub use tilo_local_identity::*;

#[cfg(feature = "local-daemons")]
mod runtime;
#[cfg(feature = "local-daemons")]
pub use runtime::{
    PhotoSource, RuntimeIdentity, install_source, load, own_public_photo, photo_for,
};

/// What the person is called: their profile name, when they have one.
#[must_use]
pub fn own_name() -> Option<String> {
    load()
        .map(|identity| identity.display_name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// The colour this person chose to wear everywhere (`identity.colour`, as their
/// own card keeps it), `None` while they chose none and every surface derives
/// theirs from their identity.
///
/// Their apps and their shell announce it where they say who they are, and every
/// surface paints them in it, so one person is one colour here, in every app
/// they edit in and on everyone else's screens.
#[must_use]
pub fn own_colour() -> Option<collab_types::presence::PresenceHue> {
    settings_store::identity::COLOUR
        .get()
        .as_deref()
        .and_then(collab_types::presence::PresenceHue::from_id)
}

#[cfg(test)]
mod name_tests {
    use super::*;
    use std::sync::Arc;

    /// Peers learn what to call this person from the name the app announces
    /// on joining: the profile name, never an id in place of one.
    #[test]
    fn an_app_names_its_person_by_their_profile_name() {
        let _guard = tilo_test_env::lock();
        let person = |name: &str| {
            install_source(Some(Arc::new(FixedIdentity(LocalIdentity {
                group_public_key: [7; 32],
                display_name: name.into(),
                avatar_color: [200, 40, 90],
                device_public_key: None,
            }))));
        };
        person("  Throwaway test identity ");
        assert_eq!(own_name().as_deref(), Some("Throwaway test identity"));
        person(" ");
        assert_eq!(own_name(), None);
        install_source(None);
    }
}
