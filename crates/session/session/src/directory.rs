//! The workspace directory on every host: the sheet listing what the sites' directories offer and
//! the workspaces this host holds, and what its presses do. What a site lists and what its door
//! grants is [`tilo_workspace_directory`]'s; what is the host's own — its site, the person a join
//! is made as, the workspaces it holds and what joining one gives it — is its [`Host`].

use tilo_desktop_surface::notifications::NotificationIconKind;
use tilo_sheets::Seat as _;
use tilo_sheets::directory::{Directory, DirectoryEffect, DirectorySeat, ListingAction};
use tilo_startup_metrics::JOIN_FLOW_TARGET;
use tilo_workspace_directory::{Held, Person};

use crate::platform::notice::{Notice, Notices};

pub(crate) use tilo_workspace_directory::{Joined, Store};

/// What the directory asks of the host it runs on. Each `Err` is the sentence the sheet says.
pub trait Host {
    /// The site whose directory is listed when the person named none.
    fn site(&self) -> Option<url::Url>;
    /// The person a join is made as.
    fn person(&self) -> Result<Person, String>;
    /// The workspaces this host holds, each by the key the sheet opens it with.
    fn held(&self) -> Vec<Held>;
    /// Go to the held workspace `key`.
    fn open(&self, key: &str) -> Result<(), String>;
    /// Give this host the workspace a join was granted.
    fn add(&self, joined: &Joined) -> Result<(), String>;
}

/// What the sheet shows: the workspaces this host holds, and what the sites' directories list.
pub(crate) fn view(store: &Store, host: &dyn Host) -> Directory {
    store.view(&host.held())
}

/// Show the workspaces this host may join or open: the ones it holds, and what the sites'
/// directories list.
pub fn open(seat: Option<&DirectorySeat>, store: &Store, host: &dyn Host) {
    let Some(seat) = seat else {
        return;
    };
    seat.clear_query();
    store.set_note(None);
    seat.show(view(store, host));
    seat.presentation().open(false);
    store.read(host.site(), None);
}

/// Carry out what the directory asked: reading a site's directory again, joining a public
/// workspace, opening one this host holds, and closing it.
///
/// Choosing Join (or Open) closes the sheet as soon as the choice has started: what became of it
/// — the workspace opening, or joining it, or a refusal — is shown where the person now is, a
/// banner, rather than a sheet left behind.
pub fn follow(
    seat: Option<&DirectorySeat>,
    store: &Store,
    notices: &dyn Notices,
    host: &dyn Host,
) {
    // A host set up to join one workspace joins it with nobody at the directory: what was granted
    // is taken and added whether or not the sheet is open.
    let answered = store.answered();
    for joined in store.take_joined() {
        match host.add(&joined) {
            Ok(()) => {
                // The person lands in the workspace they joined, once what it holds is here. A join
                // whose slot is still being admitted leaves the sheet up, saying so, and the "you
                // joined" note is posted when the host admits it, not before.
                if let Some(seat) = seat
                    && !store.settling()
                    && store.joining().is_none()
                {
                    seat.presentation().close();
                }
                if !store.settling() {
                    joined_notice(notices, &joined.name);
                }
            }
            Err(error) => {
                tracing::warn!(%error, workspace = %joined.granted.workspace, "a joined workspace was not added");
                tracing::info!(
                    target: JOIN_FLOW_TARGET,
                    workspace = %joined.granted.workspace,
                    name = %joined.name,
                    %error,
                    outcome = "slot removed",
                    "join: the desk would not make the workspace's slot, so nothing was joined"
                );
                store.failing(tilo_sheets::words(
                    "directory-join-failed",
                    &[("name", &joined.name), ("error", &error)],
                ));
            }
        }
        if let Some(seat) = seat {
            seat.show(view(store, host));
        }
    }
    // A join the person chose closed the sheet when it started, so one that failed is said where
    // the person is now.
    for said in store.take_failures_to_say() {
        failure_notice(notices, &said);
    }
    let Some(seat) = seat else {
        return;
    };
    if answered {
        seat.show(view(store, host));
    }
    for effect in seat.take_asked() {
        match effect {
            DirectoryEffect::Join { key } => join_listed(seat, store, host, &key),
            DirectoryEffect::Open { key } => open_listed(seat, store, host, &key),
            DirectoryEffect::Look { text } => {
                if let Some(site) = typed_site(&text) {
                    // A site the person typed is asked for what it lists, and the sheet says it is
                    // being read until the worker answers.
                    store.read(host.site(), Some(site));
                    seat.show(view(store, host));
                    continue;
                }
                // One workspace answers the search: Enter takes it, opening it when this host
                // holds it and joining it otherwise.
                let only: Vec<_> = seat
                    .directory()
                    .listings
                    .into_iter()
                    .filter(|listing| listing.matches(&text))
                    .collect();
                let [only] = only.as_slice() else {
                    tracing::debug!(%text, "the workspace directory looks up a site, a link or a code");
                    continue;
                };
                if only.action == ListingAction::Open {
                    open_listed(seat, store, host, &only.key);
                } else {
                    join_listed(seat, store, host, &only.key);
                }
            }
            DirectoryEffect::LinkOrCode { text } => {
                // A link or a code the person pasted joins at once: what it is decides where, and
                // the sheet says so until it lands.
                if let Err(error) = host.person().and_then(|person| store.take(&text, &person)) {
                    store.set_note(Some(error));
                }
                seat.show(view(store, host));
            }
            DirectoryEffect::Refresh => {
                store.read(host.site(), None);
                seat.show(view(store, host));
            }
            DirectoryEffect::ClearNote => {
                store.set_note(None);
                seat.show(view(store, host));
            }
            DirectoryEffect::Done => {
                seat.presentation().close();
            }
        }
    }
}

/// Go to the workspace `key` names, which this host holds, and be done with the directory. A
/// workspace the host no longer admits is said where the person picked it, and the directory stays
/// up to pick another.
fn open_listed(seat: &DirectorySeat, store: &Store, host: &dyn Host, key: &str) {
    if let Err(error) = host.open(key) {
        tracing::warn!(%error, %key, "workspace directory open");
        store.set_note(Some(error));
        seat.show(view(store, host));
        return;
    }
    seat.presentation().close();
}

/// Join the listing `key` names, one join at a time: the row joining says so, and pressing it again
/// says it is on its way.
fn join_listed(seat: &DirectorySeat, store: &Store, host: &dyn Host, key: &str) {
    let Some(listing) = store.listing(key) else {
        // The row was drawn from a list a later read replaced: answer the press, rather than leave
        // the row saying nothing.
        store.set_note(Some(tilo_sheets::word("directory-listing-gone")));
        seat.show(view(store, host));
        return;
    };
    match store.joining() {
        Some(joining) if joining == key => {
            store.set_note(Some(tilo_sheets::words(
                "directory-already-joining",
                &[("name", &listing.entry.name)],
            )));
        }
        // One join at a time: the first is still on its way.
        Some(_) => return,
        None => {
            let joined = host
                .person()
                .inspect_err(|reason| {
                    tracing::info!(
                        target: JOIN_FLOW_TARGET,
                        workspace = %listing.entry.workspace,
                        %key,
                        site = %listing.host,
                        %reason,
                        outcome = "no person to join as",
                        "join: this host has no person to join as"
                    );
                })
                .and_then(|person| store.join(key, &listing, &person));
            match joined {
                // The join is on its way: the sheet is done with it, and the person is told when it
                // lands (or why it did not).
                Ok(()) => {
                    seat.presentation().close();
                    return;
                }
                Err(error) => store.set_note(Some(error)),
            }
        }
    }
    seat.show(view(store, host));
}

/// Say the workspace `name` is theirs now, or no longer, in `words`' own: marked as the
/// workspace's news.
pub(crate) fn workspace_notice(notices: &dyn Notices, words: (&str, &str), name: &str) {
    let (title, body) = words;
    notices.show(
        &Notice::new(
            String::new(),
            1,
            tilo_desktop_surface::i18n::t_args(title, &[("name", name.into())]),
            tilo_desktop_surface::i18n::t(body),
        )
        .about(NotificationIconKind::Workspace),
    );
}

/// Say the person joined `name`, where they are now: a note that the workspace is theirs, its
/// windows open there.
pub(crate) fn joined_notice(notices: &dyn Notices, name: &str) {
    workspace_notice(
        notices,
        ("session-joined-workspace", "session-joined-workspace-body"),
        name,
    );
}

/// Say a join the person chose did not happen, and why, where they are: the workspace directory's
/// own sentence, as a banner.
fn failure_notice(notices: &dyn Notices, said: &str) {
    notices.show(&Notice::new(
        String::new(),
        2,
        tilo_desktop_surface::i18n::t("session-join-failed-title"),
        said,
    ));
}

/// The site `text` names, when what was typed into the search is one: a word that is no address
/// looks at the list instead.
fn typed_site(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.contains('.') && !text.chars().any(char::is_whitespace))
        .then(|| text.to_owned())
}
