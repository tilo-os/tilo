//! What this host calls the person at it.
//!
//! The desk, its bar and the roster name the person they belong to: the name they chose, else the
//! name this host knows them by. A computer whose identity is in is named by its key, which is
//! never shown as a name.

/// The name this person goes by at their own desk: the one they chose, else the name the session
/// already knows them by, else the name this host knows whoever is at it by. A computer with no
/// identity of its own goes by that account, which is the session's person there, so the desk and
/// its bar name them alike; a computer whose identity is in is named by its key, which is never
/// shown as a name.
pub fn desk_person_name(chosen: Option<String>, person: &str) -> String {
    chosen_name(chosen).unwrap_or_else(|| {
        let known = person.trim();
        if known.is_empty() || is_identity_key(known) {
            account_name()
        } else {
            known.to_owned()
        }
    })
}

/// The display name `chosen` is, where it is one they gave: not blank, not the placeholder an
/// unnamed identity carries, and one a workspace may take.
fn chosen_name(chosen: Option<String>) -> Option<String> {
    chosen
        .filter(|name| given_by_person(name))
        .and_then(|name| {
            tilo_user_identity::display_name(&name)
                .ok()
                .map(str::to_owned)
        })
}

/// Whether `person` is a key rather than a name to read: the root key hex a computer's identity
/// names them by.
fn is_identity_key(person: &str) -> bool {
    person.len() == 64 && person.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether `name` is a name the person gave, rather than blank or the stand-in provisioning wrote
/// before they named themselves.
#[cfg(target_os = "linux")]
fn given_by_person(name: &str) -> bool {
    runtime_identity::is_chosen_display_name(name)
}

#[cfg(not(target_os = "linux"))]
fn given_by_person(name: &str) -> bool {
    // The page's identity carries the name the person signed in with: nothing stands one in.
    !name.trim().is_empty()
}

/// The name this host knows whoever is at it by, where the person chose none.
#[cfg(target_os = "linux")]
fn account_name() -> String {
    tilo_identity_host::provisioning::login_account_display_name()
}

/// The page has no login account: whoever is at it is named by the identity they signed in with.
#[cfg(not(target_os = "linux"))]
fn account_name() -> String {
    ui::local_identity::load().map_or_else(String::new, |identity| identity.display_name)
}
