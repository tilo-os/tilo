//! What this host calls the person at it.
//!
//! The shell, its bar and the roster name the person they belong to: the name they chose, else the
//! name this host knows them by. A computer whose identity is in is named by its key, which is
//! never shown as a name.

/// The name this person goes by at their own shell: the one they chose, else the name the session
/// already knows them by, else the name this host knows whoever is at it by. A computer with no
/// identity of its own goes by that account, which is the session's person there, so the shell and
/// its bar name them alike; a computer whose identity is in is named by its key, which is never
/// shown as a name.
pub fn person_name(chosen: Option<String>, person: &str) -> String {
    session_name(chosen, person).unwrap_or_else(account_name)
}

/// The session's name to publish in presence, or none before a person identified by a root key
/// has given a name. Sessions without an identity can still publish their account name.
pub(crate) fn session_name(chosen: Option<String>, person: &str) -> Option<String> {
    chosen_name(chosen).or_else(|| {
        let known = person.trim();
        (!known.is_empty() && !is_identity_key(known)).then(|| known.to_owned())
    })
}

/// The name `chosen` is one the person gave themselves, where it is one: `None` while they gave
/// none, so a field for their name asks rather than holding the words that stand in for one. What
/// [`person_name`] falls back to is the name this device knows them by, theirs to show others,
/// never theirs to edit.
pub fn given_name(chosen: Option<String>) -> Option<String> {
    chosen_name(chosen)
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
    tilo_identity::store::is_chosen_display_name(name)
}

#[cfg(not(target_os = "linux"))]
fn given_by_person(name: &str) -> bool {
    // The page's identity carries the name the person signed in with: nothing stands one in.
    !name.trim().is_empty()
}

/// The name this host knows whoever is at it by, where the person chose none.
#[cfg(target_os = "linux")]
fn account_name() -> String {
    tilo_identity::provisioning::login_account_display_name()
}

/// The page has no login account: whoever is at it is named by the identity they signed in with.
#[cfg(not(target_os = "linux"))]
fn account_name() -> String {
    ::collaboration::local_identity::load()
        .map_or_else(String::new, |identity| identity.display_name)
}
