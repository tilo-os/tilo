//! Where the person's windows were lately, newest first, each reopening
//! where they left off: on this device, and, offered by the runtime peer,
//! on their other devices ("continue from the laptop").
//!
//! A place is each window's location as its app declared it to the window
//! bar, with the view's anchor when the app gave one. The places are the
//! session's own record ([`RecentPlaces`]), kept with the rest of what the
//! session holds, never a file of the search's. A row with an anchor opens
//! through the app's `open` at it, by address, the app launched first when
//! it is closed; without one, the window it was in comes forward while it
//! lasts, else the app opens.

use serde::{Deserialize, Serialize};
use ui::search::FieldWeight;
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind};

use crate::model::{Hit, InvokeTarget, Source, Target};

/// How many places are kept: the newest, far more than are shown, so those
/// of other workspaces leave each workspace its own.
pub const KEPT: usize = 64;
/// How many places are listed while nothing is typed.
const LISTED: usize = 6;

/// One place a window was, as its app declared it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    /// The app showing it.
    pub app: String,
    /// The room of the workspace it was in.
    pub room: String,
    /// The window it was in, while that window lasts.
    pub tab: u64,
    /// What the window bar showed: the document, the page, the folder.
    pub label: String,
    /// Its whole address, when the app gave one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    /// The view's anchor, as the app's `open` takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// When the window was last there, in seconds since the epoch.
    pub at: u64,
    /// The other device it was on, by name; `None` for this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

impl Place {
    fn names(&self) -> &str {
        self.anchor
            .as_deref()
            .filter(|anchor| !anchor.is_empty())
            .or_else(|| (!self.value.is_empty()).then_some(self.value.as_str()))
            .unwrap_or(&self.label)
    }

    /// The same place again: its app, its workspace, its device and what it
    /// names.
    fn same_as(&self, other: &Self) -> bool {
        self.app == other.app
            && self.room == other.room
            && self.device == other.device
            && self.names() == other.names()
    }

    fn row_id(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.app,
            self.room,
            self.device.as_deref().unwrap_or_default(),
            self.names()
        )
    }
}

/// The places the session keeps, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentPlaces(Vec<Place>);

impl RecentPlaces {
    /// A window was at `place`: it moves to the front, the oldest beyond
    /// [`KEPT`] going.
    pub fn record(&mut self, place: Place) {
        self.0.retain(|kept| !kept.same_as(&place));
        self.0.insert(0, place);
        self.0.truncate(KEPT);
    }

    #[must_use]
    pub fn places(&self) -> &[Place] {
        &self.0
    }
}

/// The kind places are grouped under, in the desktop's language.
#[must_use]
pub fn continue_kind() -> ResourceKind {
    ResourceKind::Custom(crate::i18n::t("search-kind-continue"))
}

/// The places of one workspace, as search offers them, or of every
/// workspace when the search is widened.
pub struct Continue {
    places: Vec<Place>,
    /// The tabs still open, which a place without an anchor comes back to.
    open_tabs: Vec<u64>,
    /// The workspace search opened in: a place elsewhere says which
    /// workspace holds it, since rows of two workspaces otherwise read
    /// alike.
    home: String,
    /// The name the desk gives each workspace, by room: what a place
    /// elsewhere says.
    names: Vec<(String, String)>,
    app_name: Box<dyn Fn(&str) -> String>,
}

impl Continue {
    /// The places of `recent` in `room`, with `open_tabs` the windows still
    /// open and `app_name` an app's name as people know it.
    pub fn new(
        recent: &RecentPlaces,
        room: &str,
        open_tabs: Vec<u64>,
        app_name: impl Fn(&str) -> String + 'static,
    ) -> Self {
        Self {
            places: recent
                .places()
                .iter()
                .filter(|place| place.room == room)
                .cloned()
                .collect(),
            open_tabs,
            home: room.to_owned(),
            names: Vec::new(),
            app_name: Box::new(app_name),
        }
    }

    /// The places of every workspace of `recent`, each naming the workspace
    /// it is in where that is not `home` ("in Lounge"), which the desk
    /// gives as `(room, name)` pairs.
    pub fn everywhere(
        recent: &RecentPlaces,
        home: &str,
        open_tabs: Vec<u64>,
        names: Vec<(String, String)>,
        app_name: impl Fn(&str) -> String + 'static,
    ) -> Self {
        Self {
            places: recent.places().to_vec(),
            open_tabs,
            home: home.to_owned(),
            names,
            app_name: Box::new(app_name),
        }
    }

    fn hit(&self, place: &Place, score: i32) -> Hit {
        let target = match (&place.anchor, &place.device) {
            (Some(anchor), _) if !anchor.is_empty() => Target::Invoke(Box::new(
                InvokeTarget::open(&place.app, &place.room, anchor),
            )),
            (_, None) if self.open_tabs.contains(&place.tab) => Target::Window {
                tab: place.tab,
                app: Some(place.app.clone()),
            },
            _ => Target::Launch {
                app: place.app.clone(),
            },
        };
        let mut facts = vec![(self.app_name)(&place.app)];
        if place.room != self.home {
            facts.push(
                match self
                    .names
                    .iter()
                    .find(|(room, _)| *room == place.room)
                    .map(|(_, name)| name)
                {
                    Some(name) => crate::i18n::t_args(
                        "search-continue-in-workspace",
                        &[("workspace", name.as_str().into())],
                    ),
                    None => crate::i18n::t("search-continue-in-another-workspace"),
                },
            );
        }
        if let Some(device) = &place.device {
            facts.push(crate::i18n::t_args(
                "search-continue-on-device",
                &[("device", device.as_str().into())],
            ));
        }
        Hit {
            kind: continue_kind(),
            provider: self.id().into(),
            id: place.row_id(),
            title: place.label.clone(),
            subtitle: None,
            facts,
            score,
            icon: None,
            target,
        }
    }
}

impl Source for Continue {
    fn id(&self) -> &'static str {
        "continue"
    }

    fn name(&self) -> String {
        crate::i18n::t("search-kind-continue")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return self
                .places
                .iter()
                .take(LISTED)
                .enumerate()
                .map(|(at, place)| {
                    let at = i32::try_from(at).unwrap_or(MAX_PROVIDER_SCORE);
                    self.hit(place, MAX_PROVIDER_SCORE.saturating_sub(at))
                })
                .collect();
        }
        self.places
            .iter()
            .filter_map(|place| {
                let app = (self.app_name)(&place.app);
                let score = ui::search::system::rank_fields(
                    needle,
                    &[
                        (&place.label, FieldWeight::TITLE),
                        (&app, FieldWeight::KEYWORD),
                    ],
                )?;
                Some(self.hit(place, score))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(label: &str, anchor: Option<&str>, at: u64) -> Place {
        Place {
            app: "so.tilo.notes".into(),
            room: "room".into(),
            tab: 4,
            label: label.into(),
            value: String::new(),
            anchor: anchor.map(str::to_owned),
            at,
            device: None,
        }
    }

    #[test]
    fn coming_back_to_a_place_moves_it_to_the_front_and_only_the_newest_stay() {
        let mut recent = RecentPlaces::default();
        recent.record(place("Groceries", Some("n1"), 1));
        recent.record(place("Ideas", Some("n2"), 2));
        recent.record(place("Groceries", Some("n1"), 3));
        let labels: Vec<_> = recent.places().iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["Groceries", "Ideas"]);
        for at in 0..100 {
            recent.record(place(&format!("p{at}"), None, at));
        }
        assert_eq!(recent.places().len(), KEPT);
    }

    #[test]
    fn a_place_reopens_at_its_anchor_else_its_window_else_its_app() {
        let mut recent = RecentPlaces::default();
        recent.record(place("Groceries", Some("n1"), 1));
        recent.record(place("Inbox", None, 2));
        let mut away = place("Draft", None, 3);
        away.device = Some("Laptop".into());
        away.tab = 9;
        recent.record(away);
        let shown = Continue::new(&recent, "room", vec![4], |_| "Notes".into());
        let hits = shown.query("");
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits[0].target,
            Target::Launch {
                app: "so.tilo.notes".into()
            },
            "another device's window is not here"
        );
        assert_eq!(hits[0].facts.len(), 2);
        assert_eq!(
            hits[1].target,
            Target::Window {
                tab: 4,
                app: Some("so.tilo.notes".into())
            }
        );
        let Target::Invoke(open) = &hits[2].target else {
            panic!("an anchored place opens by address");
        };
        assert_eq!(open.address.resource, "n1");
        assert_eq!(shown.query("grocer").len(), 1);
        assert!(
            Continue::new(&recent, "other", Vec::new(), |_| String::new())
                .query("")
                .is_empty()
        );
    }
}
