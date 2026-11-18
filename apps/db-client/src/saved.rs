//! What a shared workspace keeps of Database: saved connections, saved queries
//! and the SQL buffer, from `db-client-model`. A connection's password is never
//! stored here: it stays in each device's keyring.

pub use db_client_model::{SavedConnection, SavedQuery, Workspace, id_of, uuid_of};

#[cfg(test)]
pub(crate) fn adding(
    profile: &crate::model::ConnectionProfile,
    at: usize,
) -> ui::tilo_resource::Invoke<Workspace> {
    ui::tilo_resource::Invoke::edit(&Workspace::paths().connections.insert_as(
        &id_of(profile.id),
        at,
        &SavedConnection::of(profile),
    ))
    .expect("a connection is an edit")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnectionProfile, Engine};
    use ui::tilo_resource::Id;

    /// A profile goes through the shared data without its password, and an id
    /// that is not a UUID still names one item.
    #[test]
    fn a_profile_round_trips_without_its_password() {
        let mut profile = ConnectionProfile::new("Analytics", Engine::Postgres);
        profile.host = "db.internal".into();
        profile.port = 6543;
        profile.database = "analytics".into();
        profile.user = "reader".into();
        profile.password = "s3cret".into();
        let id = id_of(profile.id);
        let back = SavedConnection::of(&profile).profile(&id);
        assert_eq!(back.id, profile.id);
        assert_eq!(
            (
                back.name.as_str(),
                back.host.as_str(),
                back.port,
                back.user.as_str()
            ),
            ("Analytics", "db.internal", 6543, "reader")
        );
        assert!(
            back.password.is_empty(),
            "the password is not in the shared data"
        );
        let foreign = Id::from_raw("conn-1");
        assert_eq!(uuid_of(&foreign), uuid_of(&foreign));
        assert_ne!(uuid_of(&foreign), uuid_of(&Id::from_raw("conn-2")));
    }
}
