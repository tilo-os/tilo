//! The machine's other applications (its `.desktop` entries), found by name
//! beside Tilo's own while the person keeps their switch on
//! ([`HOST_APPS_SOURCE`]). A host app ranks one step below a Tilo app that
//! matches as well, so a confined Tilo app sorts first; choosing one starts
//! it through the host ([`HOST_APP_PREFIX`] on the launch).

use ui::search::FieldWeight;
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind};

use crate::model::{Hit, Source, Target};
use crate::preferences::SearchPreferences;

/// The source id of the host apps, which the person switches on and off on
/// its own in the search sources.
pub const HOST_APPS_SOURCE: &str = "host-applications";
/// What a launch of a host app names it by: `host:` and its desktop id.
pub const HOST_APP_PREFIX: &str = "host:";
/// How far below a Tilo app of the same match a host app ranks.
const HOST_APP_STEP: i32 = 5;

/// One application of the machine, as its desktop entry names it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostAppEntry {
    /// The entry's id (`org.gnome.TextEditor.desktop`), which starts it.
    pub desktop_id: String,
    pub name: String,
    /// What kind of app it is ("Text Editor").
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub keywords: Vec<String>,
}

/// The launch target that starts the host app `desktop_id`.
#[must_use]
pub fn launch(desktop_id: &str) -> Target {
    Target::Launch {
        app: format!("{HOST_APP_PREFIX}{desktop_id}"),
    }
}

/// The desktop id a launch names, when it names a host app.
#[must_use]
pub fn launched(app: &str) -> Option<&str> {
    app.strip_prefix(HOST_APP_PREFIX)
}

pub struct HostApps {
    entries: Vec<HostAppEntry>,
    fields: Vec<Vec<(String, FieldWeight)>>,
    ranker: ui::search::ListIndex,
}

impl HostApps {
    /// The host apps of `entries`, or none while the person switched their
    /// source off in `preferences`.
    #[must_use]
    pub fn new(entries: Vec<HostAppEntry>, preferences: &SearchPreferences) -> Self {
        let entries = if preferences.enabled(HOST_APPS_SOURCE) {
            entries
        } else {
            Vec::new()
        };
        let fields = entries
            .iter()
            .map(|app| {
                let mut fields = vec![(app.name.clone(), FieldWeight::TITLE)];
                fields.extend(
                    app.generic_name
                        .iter()
                        .map(|name| (name.clone(), FieldWeight::KEYWORD)),
                );
                fields.extend(
                    app.keywords
                        .iter()
                        .map(|word| (word.clone(), FieldWeight::KEYWORD)),
                );
                fields.extend(
                    app.comment
                        .iter()
                        .map(|comment| (comment.clone(), FieldWeight::SUBTITLE)),
                );
                fields
            })
            .collect();
        Self {
            entries,
            fields,
            ranker: ui::search::ListIndex::default()
                .with_ranks(ui::search::system::application_ranks),
        }
    }
}

impl Source for HostApps {
    fn id(&self) -> &'static str {
        HOST_APPS_SOURCE
    }

    fn name(&self) -> String {
        crate::i18n::t("search-source-host-apps")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        let ranks = crate::rank::rank_rows(&self.ranker, needle, &self.fields);
        self.entries
            .iter()
            .zip(ranks)
            .filter_map(|(app, rank)| {
                let score = rank?
                    .min(MAX_PROVIDER_SCORE)
                    .saturating_sub(HOST_APP_STEP)
                    .max(1);
                Some(Hit {
                    faces: Vec::new(),
                    outcome: None,
                    kind: ResourceKind::Application,
                    provider: self.id().into(),
                    id: app.desktop_id.clone(),
                    title: app.name.clone(),
                    subtitle: app.generic_name.clone().or_else(|| app.comment.clone()),
                    facts: Vec::new(),
                    score,
                    icon: None,
                    target: launch(&app.desktop_id),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> HostAppEntry {
        HostAppEntry {
            desktop_id: "org.gnome.TextEditor.desktop".into(),
            name: "Text Editor".into(),
            generic_name: Some("Editor".into()),
            comment: None,
            keywords: vec!["gedit".into()],
        }
    }

    #[test]
    fn a_host_app_is_found_by_name_and_keyword_and_starts_through_the_host() {
        let apps = HostApps::new(vec![editor()], &SearchPreferences::default());
        for needle in ["text editor", "gedit"] {
            let hits = apps.query(needle);
            assert_eq!(hits.len(), 1, "{needle}");
            assert_eq!(hits[0].target, launch("org.gnome.TextEditor.desktop"));
            assert!(hits[0].score < MAX_PROVIDER_SCORE, "below a Tilo app");
        }
        assert_eq!(launched("host:a.desktop"), Some("a.desktop"));
        assert!(apps.query("").is_empty());
    }

    #[test]
    fn the_persons_switch_leaves_host_apps_out() {
        let mut preferences = SearchPreferences::default();
        preferences.set_enabled(HOST_APPS_SOURCE, false);
        assert!(
            HostApps::new(vec![editor()], &preferences)
                .query("text")
                .is_empty()
        );
    }
}
