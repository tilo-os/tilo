//! What a window tells the instance it shows it calls people: the names presence knows them by
//! ([`said`]), which the instance's views show for the ids the record holds.

use ui::presence::PresenceCtx;

/// What this device calls its person, when presence does not say.
fn own_name() -> String {
    ui::local_identity::own_name().unwrap_or_else(|| "You".to_owned())
}

/// The workspace's other participants as presence names them, by id.
fn others(presence: &PresenceCtx) -> Vec<(String, String)> {
    let mut others: Vec<(String, String)> = presence
        .names
        .keys()
        .map(|user| (user.0.clone(), presence.display_name(user)))
        .collect();
    others.sort();
    others
}

/// What a window tells an instance running out of its process it calls people
/// (`ToApp::Names`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    /// Its own person.
    pub me: String,
    /// The others, by id.
    pub others: Vec<(String, String)>,
    /// The person each participant presence pairs is a device of, by id: the workspace's policy
    /// names people.
    pub persons: Vec<(String, String)>,
}

/// What a window with `presence` tells an instance running out of its process it calls people.
#[must_use]
pub fn said(presence: &PresenceCtx) -> Said {
    let persons = presence
        .roster()
        .into_iter()
        .filter_map(|user| {
            let id = user.id.key();
            let person = presence.person_of(&ui::permissions::UserId(id.clone()))?;
            Some((id, person.0.clone()))
        })
        .collect();
    Said {
        me: presence.self_label().unwrap_or_else(own_name),
        others: others(presence),
        persons,
    }
}
