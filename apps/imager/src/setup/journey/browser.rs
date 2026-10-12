//! Independent, consented browser domains; never inferred from file/account choices.
use std::io;
use std::path::Path;
use std::sync::Arc;

use browser_continuity::{Plan, Profile};
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{Checkbox, Column, Element, Text};

use crate::setup::account_transfer::Candidate;
use crate::setup::browser_profiles::{self, ReviewedProfile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Domain {
    Bookmarks,
    History,
}
#[derive(Clone, Default)]
pub(crate) struct SelectedSources {
    profiles: Vec<(Arc<ReviewedProfile>, Profile)>,
}
impl SelectedSources {
    #[cfg(test)]
    pub(crate) fn from_reviewed(
        profiles: Vec<(Arc<ReviewedProfile>, Profile)>,
    ) -> io::Result<Self> {
        for (index, (source, selected)) in profiles.iter().enumerate() {
            selected.validate().map_err(io::Error::other)?;
            if source.profile.id != selected.id
                || source.profile.provider != selected.provider
                || source.profile.relative_path != selected.relative_path
                || (selected.bookmarks && !source.profile.bookmarks)
                || (selected.history && !source.profile.history)
                || profiles
                    .iter()
                    .take(index)
                    .any(|(_, prior)| prior.id == selected.id)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Browser selection differs from its reviewed source",
                ));
            }
        }
        if profiles.len() > browser_continuity::MAX_PROFILES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Too many selected browser sources",
            ));
        }
        Ok(Self { profiles })
    }
    pub(crate) fn profiles(&self) -> &[(Arc<ReviewedProfile>, Profile)] {
        &self.profiles
    }
    pub(crate) fn plan(&self, binding: String, identity: [u8; 32]) -> io::Result<Plan> {
        Plan::new(
            binding,
            identity,
            self.profiles
                .iter()
                .map(|(_, profile)| profile.clone())
                .collect(),
        )
        .map_err(io::Error::other)
    }
}
pub(super) struct Selection {
    profiles: Vec<Arc<ReviewedProfile>>,
    bookmarks: bool,
    history: bool,
}
impl Default for Selection {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            bookmarks: true,
            history: true,
        }
    }
}
impl Selection {
    /// The caller runs this on its consented discovery worker, not the UI thread.
    pub(super) fn discover(home: &Path, candidates: &[Candidate]) -> io::Result<Self> {
        Ok(Self {
            profiles: browser_profiles::discover(home, candidates)?
                .into_iter()
                .map(Arc::new)
                .collect(),
            ..Self::default()
        })
    }
    pub(super) fn available(&self) -> bool {
        !self.profiles.is_empty()
    }
    pub(super) fn chosen(&self, domain: Domain) -> bool {
        match domain {
            Domain::Bookmarks => self.bookmarks,
            Domain::History => self.history,
        }
    }
    pub(super) fn set(&mut self, domain: Domain, included: bool) {
        match domain {
            Domain::Bookmarks => self.bookmarks = included,
            Domain::History => self.history = included,
        }
    }
    pub(super) fn selected(&self) -> SelectedSources {
        SelectedSources {
            profiles: self
                .profiles
                .iter()
                .filter_map(|source| {
                    let mut profile = source.profile.clone();
                    profile.bookmarks &= self.bookmarks;
                    profile.history &= self.history;
                    (profile.bookmarks || profile.history).then(|| (Arc::clone(source), profile))
                })
                .collect(),
        }
    }
    /// Both domains remain independent of Documents/Files and replacement logins.
    pub(super) fn view(&self, bindings: impl Fn(Domain, bool) + 'static + Clone) -> Element {
        let labels = self
            .profiles
            .iter()
            .map(|source| source.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let mut body = Column::new()
            .with_no_padding()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(
                Text::new(&super::t("installer-browser-title")).with_role(TextRole::Subheading),
            ))
            .push(Box::new(Text::new(&labels).with_role(TextRole::Caption)));
        for (domain, key, id) in [
            (
                Domain::Bookmarks,
                "installer-browser-bookmarks",
                "installer.browser.bookmarks",
            ),
            (
                Domain::History,
                "installer-browser-history",
                "installer.browser.history",
            ),
        ] {
            let available = self.profiles.iter().any(|source| match domain {
                Domain::Bookmarks => source.profile.bookmarks,
                Domain::History => source.profile.history,
            });
            if !available {
                continue;
            }
            let checked = ui::LocalState::new(self.chosen(domain));
            body = body.push(Box::new(
                Checkbox::from_state(&checked)
                    .with_id(id)
                    .with_label(super::t(key))
                    .on_change({
                        let changed = bindings.clone();
                        move |included| changed(domain, included)
                    }),
            ));
        }
        Box::new(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn browsing_domain_selection_captures_only_its_actual_reviewed_files() {
        let home = tempfile::tempdir().unwrap();
        let profile = home.path().join(".config/chromium/Default");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("Bookmarks"), b"bookmarks").unwrap();
        fs::write(profile.join("History"), b"private history").unwrap();
        fs::write(profile.join("Cookies"), b"not consented").unwrap();
        let inventory = crate::setup::account_transfer::discover(
            &crate::setup::account_transfer::SourceRoots {
                home: home.path().into(),
                config: home.path().join(".config"),
            },
        );
        let mut selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        assert!(selection.available());
        assert!(selection.chosen(Domain::Bookmarks));
        assert!(selection.chosen(Domain::History));
        selection.set(Domain::History, false);
        let selected = selection.selected();
        let plan = selected.plan("exact-setup".into(), [3; 32]).unwrap();
        assert_eq!(plan.profiles.len(), 1);
        assert!(!plan.profiles[0].history);
        assert_eq!(plan.profiles[0].allowed_files(), ["Bookmarks"]);
        let (source, profile) = &selected.profiles()[0];
        let captured = source.capture_files(profile).unwrap();
        assert_eq!(captured.files().len(), 1);
        assert_eq!(captured.files()[0].name(), "Bookmarks");
        assert_eq!(captured.files()[0].bytes(), b"bookmarks");
        selection.set(Domain::Bookmarks, false);
        assert!(selection.selected().profiles().is_empty());
        assert_eq!(
            fs::read(home.path().join(".config/chromium/Default/History")).unwrap(),
            b"private history"
        );
        assert_eq!(
            fs::read(home.path().join(".config/chromium/Default/Cookies")).unwrap(),
            b"not consented"
        );
    }
}
