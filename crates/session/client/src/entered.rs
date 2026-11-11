//! The workspaces a device entered, as its person's switcher offers them: the rules of that list,
//! kept wherever the device keeps it (the page, in its browser's storage).
//!
//! A workspace is remembered with the host that held it (that host's device key), so one another
//! host now answers for is forgotten rather than offered. Remembering grants nothing: opening one
//! is the workspace's own admission again. The workspace a bare entry reopens (the one last
//! chosen) stops being chosen once it is forgotten or failed to open.

use serde::{Deserialize, Serialize};

/// The most workspaces remembered; the least recently opened go first.
pub const MAX_ENTERED: usize = 32;

/// One workspace a device entered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entered {
    /// The origin its host is reached at.
    pub origin: String,
    /// The workspace's id.
    pub room: String,
    /// The device key (hex) of the host that held it when it was entered.
    pub host_device: String,
    /// What the person saw it called, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The last attempt to open it failed without its host saying it is gone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unavailable: bool,
}

/// A workspace by where it is: its host's origin and its id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub origin: String,
    pub room: String,
}

impl Entered {
    fn is(&self, place: &Place) -> bool {
        self.origin == place.origin && self.room == place.room
    }

    fn place(&self) -> Place {
        Place {
            origin: self.origin.clone(),
            room: self.room.clone(),
        }
    }

    /// Whether it names a workspace and the host that held it: one that does not is never offered,
    /// as nothing could ever tell whether it still exists.
    #[must_use]
    pub fn kept(&self) -> bool {
        !self.room.is_empty() && !self.origin.is_empty() && host_device(&self.host_device)
    }
}

/// Whether `key` is a host's device key, as hex.
fn host_device(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// What a change to the list did: the list as it is now, and the workspaces that stop being
/// the one a bare entry reopens (forgotten, or failed to open).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Changed {
    pub entered: Vec<Entered>,
    pub unchoose: Vec<Place>,
}

/// What opening a remembered workspace that failed says of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Settled {
    /// Another host answers at its origin now: it is gone with the host that held it, forgotten.
    Gone,
    /// Nothing says it is gone (the same host, or no answer at all): it stays, marked as not
    /// opening last time.
    Unavailable,
    /// Not a workspace this device remembers.
    Unknown,
    /// Its origin said its owner deleted it: forgotten.
    Deleted,
}

/// `list` as kept: what is no entry dropped, at most [`MAX_ENTERED`].
fn kept(mut list: Vec<Entered>) -> Vec<Entered> {
    list.retain(Entered::kept);
    list.truncate(MAX_ENTERED);
    list
}

/// The host at `origin` answered as `host_device`: the workspaces remembered there under another
/// host are gone with that host's state, so they are forgotten. Only an answer from the origin
/// itself is passed here, never a failure to reach it.
#[must_use]
pub fn forget_gone(list: Vec<Entered>, origin: &str, host_device_key: &str) -> Changed {
    if !host_device(host_device_key) {
        return Changed {
            entered: kept(list),
            unchoose: Vec::new(),
        };
    }
    let (gone, entered): (Vec<_>, Vec<_>) = kept(list)
        .into_iter()
        .partition(|entry| entry.origin == origin && entry.host_device != host_device_key);
    Changed {
        entered,
        unchoose: gone.iter().map(Entered::place).collect(),
    }
}

/// Remember that this device opened `entry` as a member, first in the list, under its name when
/// one is known (an earlier name stays otherwise). Workspaces remembered at its origin under
/// another host are gone with it, and forgotten.
#[must_use]
pub fn remember(list: Vec<Entered>, entry: Entered) -> Changed {
    if !entry.kept() {
        return Changed {
            entered: kept(list),
            unchoose: Vec::new(),
        };
    }
    let Changed { entered, unchoose } = forget_gone(list, &entry.origin, &entry.host_device);
    let place = entry.place();
    let earlier = entered
        .iter()
        .find(|kept| kept.is(&place))
        .and_then(|kept| kept.name.clone());
    let first = Entered {
        name: entry.name.filter(|name| !name.is_empty()).or(earlier),
        unavailable: false,
        ..entry
    };
    let mut list = vec![first];
    list.extend(entered.into_iter().filter(|kept| !kept.is(&place)));
    Changed {
        entered: kept(list),
        unchoose,
    }
}

/// Name a remembered workspace after the workspace it showed, if it has no name yet.
#[must_use]
pub fn name(list: Vec<Entered>, place: &Place, name: &str) -> Changed {
    let mut list = kept(list);
    if let Some(entry) = list
        .iter_mut()
        .find(|entry| entry.is(place) && entry.name.is_none())
        && !name.is_empty()
    {
        entry.name = Some(name.to_owned());
    }
    Changed {
        entered: list,
        unchoose: Vec::new(),
    }
}

/// The last attempt to open `place` failed: say so where it is offered, and stop reopening it on
/// a bare entry.
#[must_use]
pub fn mark_unavailable(list: Vec<Entered>, place: &Place) -> Changed {
    let mut entered = kept(list);
    let Some(entry) = entered.iter_mut().find(|entry| entry.is(place)) else {
        return Changed {
            entered,
            unchoose: Vec::new(),
        };
    };
    entry.unavailable = true;
    Changed {
        entered,
        unchoose: vec![place.clone()],
    }
}

/// The host of `place` said its owner deleted it: it is forgotten, and a bare entry no longer
/// reopens it.
#[must_use]
pub fn forget_deleted(list: Vec<Entered>, place: &Place) -> Changed {
    let (deleted, entered): (Vec<_>, Vec<_>) =
        kept(list).into_iter().partition(|entry| entry.is(place));
    Changed {
        entered,
        unchoose: deleted.iter().map(Entered::place).collect(),
    }
}

/// Opening the remembered `place` failed, and its origin answers as `answering` (the device key
/// of the host there now), or did not answer at all (`None`), and says whether its owner
/// `deleted` it (`false` when it did not answer). A deleted workspace, or another host than the
/// one that held it, means it is gone; anything else says nothing about it.
#[must_use]
pub fn settle_failed(
    list: Vec<Entered>,
    place: &Place,
    answering: Option<&str>,
    deleted: bool,
) -> (Changed, Settled) {
    let list = kept(list);
    let Some(entry) = list.iter().find(|entry| entry.is(place)).cloned() else {
        return (
            Changed {
                entered: list,
                unchoose: Vec::new(),
            },
            Settled::Unknown,
        );
    };
    if deleted {
        return (forget_deleted(list, place), Settled::Deleted);
    }
    match answering.filter(|key| host_device(key)) {
        Some(answering) if answering != entry.host_device => {
            (forget_gone(list, &entry.origin, answering), Settled::Gone)
        }
        _ => (mark_unavailable(list, place), Settled::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "abababababababababababababababababababababababababababababababab";
    const OTHER: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

    fn entry(origin: &str, room: &str, host: &str) -> Entered {
        Entered {
            origin: origin.into(),
            room: room.into(),
            host_device: host.into(),
            name: None,
            unavailable: false,
        }
    }

    fn place(origin: &str, room: &str) -> Place {
        Place {
            origin: origin.into(),
            room: room.into(),
        }
    }

    #[test]
    fn a_deleted_workspace_is_forgotten_and_no_other() {
        let list = remember(Vec::new(), entry("https://a", "kept", HOST)).entered;
        let list = remember(list, entry("https://a", "deleted", HOST)).entered;
        let changed = forget_deleted(list, &place("https://a", "deleted"));
        assert_eq!(changed.unchoose, [place("https://a", "deleted")]);
        assert_eq!(changed.entered.len(), 1);
        assert_eq!(changed.entered[0].room, "kept");
        let again = forget_deleted(changed.entered, &place("https://a", "deleted"));
        assert!(
            again.unchoose.is_empty(),
            "one not remembered changes nothing"
        );
    }

    #[test]
    fn the_last_opened_comes_first_keeps_its_name_and_the_list_is_bounded() {
        let mut list = Vec::new();
        for room in 0..40 {
            list = remember(list, entry("https://a", &room.to_string(), HOST)).entered;
        }
        assert_eq!(list.len(), MAX_ENTERED);
        assert_eq!(list[0].room, "39");
        let named = name(list, &place("https://a", "39"), "Acme").entered;
        let again = remember(named, entry("https://a", "39", HOST)).entered;
        assert_eq!(
            again[0].name.as_deref(),
            Some("Acme"),
            "an earlier name stays"
        );
        assert_eq!(
            remember(again.clone(), entry("https://a", "", HOST)).entered,
            again,
            "a damaged entry is never remembered"
        );
    }

    #[test]
    fn a_host_set_up_again_forgets_what_the_one_before_it_held() {
        let list = remember(Vec::new(), entry("https://a", "1", HOST)).entered;
        let list = remember(list, entry("https://b", "2", HOST)).entered;
        let changed = remember(list, entry("https://a", "3", OTHER));
        assert_eq!(
            changed
                .entered
                .iter()
                .map(|entry| entry.room.as_str())
                .collect::<Vec<_>>(),
            ["3", "2"]
        );
        assert_eq!(changed.unchoose, [place("https://a", "1")]);
    }

    #[test]
    fn a_failed_open_forgets_only_what_another_host_now_answers_for() {
        let list = remember(Vec::new(), entry("https://a", "1", HOST)).entered;
        let (changed, settled) = settle_failed(list.clone(), &place("https://a", "1"), None, false);
        assert_eq!(settled, Settled::Unavailable);
        assert!(changed.entered[0].unavailable);
        assert_eq!(
            changed.unchoose,
            [place("https://a", "1")],
            "a bare entry stops reopening it"
        );
        let (changed, settled) =
            settle_failed(list.clone(), &place("https://a", "1"), Some(HOST), false);
        assert_eq!(settled, Settled::Unavailable);
        assert_eq!(changed.entered.len(), 1);
        let (changed, settled) =
            settle_failed(list.clone(), &place("https://a", "1"), Some(OTHER), false);
        assert_eq!(settled, Settled::Gone);
        assert!(changed.entered.is_empty());
        // The host that held it says its owner deleted it: forgotten.
        let (changed, settled) =
            settle_failed(list.clone(), &place("https://a", "1"), Some(HOST), true);
        assert_eq!(settled, Settled::Deleted);
        assert!(changed.entered.is_empty());
        assert_eq!(changed.unchoose, [place("https://a", "1")]);
        assert_eq!(
            settle_failed(list, &place("https://a", "9"), None, false).1,
            Settled::Unknown
        );
    }
}
