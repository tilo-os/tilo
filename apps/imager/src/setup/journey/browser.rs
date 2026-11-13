//! Presentation for independent, consented browser domains.

use imager_backend::account_transfer::Candidate;
pub(super) use imager_backend::browser_selection::Domain;
pub(crate) use imager_backend::browser_selection::SelectedSources;
use std::{io, path::Path};
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{Checkbox, Column, Element, Text};

#[derive(Default)]
pub(super) struct Selection {
    authority: imager_backend::browser_selection::Selection,
}
impl Selection {
    pub(super) fn discover(home: &Path, candidates: &[Candidate]) -> io::Result<Self> {
        Ok(Self {
            authority: imager_backend::browser_selection::Selection::discover(home, candidates)?,
        })
    }
    pub(super) fn available(&self) -> bool {
        self.authority.available()
    }
    pub(super) fn chosen(&self, domain: Domain) -> bool {
        self.authority.chosen(domain)
    }
    pub(super) fn set(&mut self, domain: Domain, included: bool) {
        self.authority.set(domain, included);
    }
    pub(super) fn selected(&self) -> SelectedSources {
        self.authority.selected()
    }
    pub(super) fn inventory(&self) -> &[imager_backend::browser_inventory::ProfileInventory] {
        self.authority.inventory()
    }
    pub(super) fn inventory_incomplete(&self) -> bool {
        self.authority.inventory_incomplete()
    }
    pub(super) fn inventory_report(&self) -> imager_backend::browser_inventory::Report {
        self.authority.inventory_report()
    }
    pub(super) fn set_profile(&mut self, id: &str, included: bool) -> Result<(), &'static str> {
        self.authority.set_profile(id, included)
    }
    pub(super) fn profile_state(
        &self,
        id: &str,
    ) -> Option<imager_backend::browser_selection::ProfileState> {
        self.authority.profile_state(id)
    }
    pub(super) fn profile_included(&self, id: &str) -> bool {
        self.authority.profile_included(id)
    }
    pub(super) fn set_profile_domain(
        &mut self,
        id: &str,
        domain: Domain,
        included: bool,
    ) -> Result<bool, &'static str> {
        self.authority.set_profile_domain(id, domain, included)
    }
    pub(super) fn profile_choices(
        &self,
    ) -> Vec<(String, imager_backend::browser_selection::ProfileState)> {
        self.authority.profile_states()
    }
    pub(super) fn restore_profiles(
        &mut self,
        choices: &[(String, imager_backend::browser_selection::ProfileState)],
    ) {
        self.authority.restore_states(choices);
    }
    pub(super) fn adopt(&mut self, previous: &Self) {
        self.authority.adopt(&previous.authority);
    }
    pub(super) fn view(
        &self,
        actions: Option<&ui::framework::Actions<crate::resource::Imager>>,
    ) -> Element {
        let labels = self
            .authority
            .profiles()
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
            let available = self.authority.profiles().any(|source| match domain {
                Domain::Bookmarks => source.profile.bookmarks,
                Domain::History => source.profile.history,
            });
            if !available {
                continue;
            }
            let checked = ui::LocalState::new(self.chosen(domain));
            let category = match domain {
                Domain::Bookmarks => "bookmarks",
                Domain::History => "history",
            };
            body = body.push(Box::new(super::bound(
                actions,
                Checkbox::from_state(&checked)
                    .with_id(id)
                    .with_label(super::t(key)),
                super::InstallerPreviewEffect::SET_PREVIEW_CATEGORY
                    .map(move |included: bool| (category.to_owned(), included)),
            )));
        }
        Box::new(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_choices_without_reviewed_sources_never_create_source_authority() {
        let mut selection = Selection::default();
        assert!(!selection.available());
        for domain in [Domain::Bookmarks, Domain::History] {
            selection.set(domain, false);
            assert!(!selection.chosen(domain));
            selection.set(domain, true);
            assert!(selection.chosen(domain));
        }
        assert!(!selection.available());
        assert!(selection.selected().profiles().is_empty());
    }

    #[cfg(not(unix))]
    #[test]
    fn unsupported_discovery_is_an_error_not_an_empty_success() {
        let home = tempfile::tempdir().unwrap();
        let error = Selection::discover(home.path(), &[])
            .err()
            .expect("unsupported source custody must fail");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
}
