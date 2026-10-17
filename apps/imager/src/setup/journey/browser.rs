//! Presentation for independent, consented browser domains.
//! Backend discovery admits sources or reports unsupported custody on every host.
use crate::setup::account_transfer::Candidate;
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
    /// The caller runs this on its consented discovery worker, not the UI thread.
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
    /// Both domains remain independent of Documents/Files and replacement logins.
    pub(super) fn view(&self, bindings: impl Fn(Domain, bool) + 'static + Clone) -> Element {
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
