//! The preview's local decisions. No identity, filesystem or installation authority.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Destination,
    /// Compatibility of the computer that will receive a prepared install.
    Target,
    Migration,
    Identity,
    Profile,
    Password,
    Connect,
    Devices,
    Purpose,
    Belongings,
    Accounts,
    Storage,
    Review,
    Ready,
}

impl Page {
    pub fn key(self) -> &'static str {
        match self {
            Self::Destination => "destination",
            Self::Target => "target",
            Self::Migration => "migration",
            Self::Identity => "identity",
            Self::Profile => "profile",
            Self::Password => "password",
            Self::Connect => "connect",
            Self::Devices => "devices",
            Self::Purpose => "purpose",
            Self::Belongings => "belongings",
            Self::Accounts => "accounts",
            Self::Storage => "storage",
            Self::Review => "review",
            Self::Ready => "ready",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Computer,
    Another,
    Desktop,
}

impl Destination {
    pub fn key(self) -> &'static str {
        match self {
            Self::Computer => "computer",
            Self::Another => "another",
            Self::Desktop => "desktop",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    New,
    Existing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewSection {
    Destination,
    Profile,
    Identity,
    Password,
    Devices,
    Purpose,
    Belongings,
}

impl ReviewSection {
    pub fn key(self) -> &'static str {
        match self {
            Self::Destination => "destination",
            Self::Profile => "profile",
            Self::Identity => "identity",
            Self::Password => "password",
            Self::Devices => "devices",
            Self::Purpose => "purpose",
            Self::Belongings => "belongings",
        }
    }
}

#[derive(Clone)]
struct ReviewEdit {
    section: ReviewSection,
    start: Page,
    original: Box<Journey>,
}

#[derive(Clone)]
pub struct Journey {
    pub page: Page,
    pub destination: Option<Destination>,
    pub migrate: Option<bool>,
    pub identity: Option<Identity>,
    pub name: String,
    pub device_name: String,
    pub purpose: String,
    pub password: String,
    pub confirmation: String,
    pub transfer: Option<String>,
    pub categories: [bool; 3],
    pub browser_domains: [bool; 2],
    pub excluded_flatpak_apps: std::collections::BTreeSet<String>,
    pub details: bool,
    pub history: Vec<Page>,
    editing: Option<ReviewEdit>,
}

impl Journey {
    pub fn new(name: String) -> Self {
        Self {
            page: Page::Destination,
            destination: None,
            migrate: None,
            identity: None,
            name,
            device_name: String::new(),
            purpose: install_profile_model::default_purpose(),
            password: String::new(),
            confirmation: String::new(),
            transfer: None,
            categories: [true; 3],
            browser_domains: [true; 2],
            excluded_flatpak_apps: std::collections::BTreeSet::default(),
            details: false,
            history: Vec::new(),
            editing: None,
        }
    }

    pub(super) fn draft_choices(&self) -> super::draft::PublicChoices {
        use super::draft::DestinationChoice;
        super::draft::PublicChoices {
            destination: self.destination.map(|value| match value {
                Destination::Computer => DestinationChoice::Computer,
                Destination::Another => DestinationChoice::Another,
                Destination::Desktop => DestinationChoice::Desktop,
            }),
            migrate: self.migrate,
            categories: self.categories,
            browser_domains: self.browser_domains,
            excluded_flatpak_apps: self.excluded_flatpak_apps.clone(),
            device_label: self.device_name.clone(),
            purpose: self.purpose.clone(),
            deferred: self.password.is_empty(),
            target: None,
        }
    }

    pub(super) fn restore_draft(
        &mut self,
        choices: &super::draft::PublicChoices,
        kind: super::draft::IdentityKind,
        name: String,
    ) {
        use super::draft::{DestinationChoice, IdentityKind};
        *self = Self::new(name);
        self.destination = choices.destination.map(|value| match value {
            DestinationChoice::Computer => Destination::Computer,
            DestinationChoice::Another => Destination::Another,
            DestinationChoice::Desktop => Destination::Desktop,
        });
        if self.destination == Some(Destination::Desktop) {
            self.reset_for_destination(Destination::Desktop);
            self.page = Page::Review;
            self.history = vec![Page::Destination];
            return;
        }
        self.migrate = choices.migrate;
        self.categories = choices.categories;
        self.browser_domains = choices.browser_domains;
        self.excluded_flatpak_apps
            .clone_from(&choices.excluded_flatpak_apps);
        self.device_name.clone_from(&choices.device_label);
        self.purpose.clone_from(&choices.purpose);
        self.identity = Some(if kind == IdentityKind::New {
            Identity::New
        } else {
            Identity::Existing
        });
        self.transfer = match kind {
            IdentityKind::New => None,
            IdentityKind::Backup => Some("backup".into()),
            IdentityKind::Enrolled => Some("device".into()),
            IdentityKind::CurrentDevice => Some("current".into()),
        };
        // New interactive preparations do not carry a password. The old
        // unlock page is only restored for an account checkpoint that was
        // written by a build which still encrypted it.
        self.page = if choices.deferred {
            Page::Devices
        } else {
            Page::Password
        };
        self.history = vec![
            Page::Destination,
            Page::Migration,
            Page::Identity,
            if kind == IdentityKind::New {
                Page::Profile
            } else {
                Page::Connect
            },
        ];
    }

    fn reset_for_destination(&mut self, destination: Destination) {
        *self = Self::new(String::new());
        self.destination = Some(destination);
        if destination == Destination::Desktop {
            self.categories = [false; 3];
        }
    }

    pub fn choose(&mut self, choice: &str, linux: bool) -> Result<(), &'static str> {
        if self.page == Page::Destination {
            let destination = match choice {
                "computer" => Destination::Computer,
                "another" => Destination::Another,
                "desktop" if linux => Destination::Desktop,
                _ => return Err("installer-preview-invalid-choice"),
            };
            if self.destination != Some(destination) {
                if destination == Destination::Desktop
                    || self.destination == Some(Destination::Desktop)
                {
                    self.reset_for_destination(destination);
                } else {
                    self.destination = Some(destination);
                }
            }
            return Ok(());
        }
        match (self.page, choice) {
            (Page::Purpose, "computer" | "server") => self.purpose = choice.into(),
            (Page::Migration, "migrate") => self.migrate = Some(true),
            (Page::Migration, "fresh") => self.migrate = Some(false),
            (Page::Identity, "new") => {
                self.identity = Some(Identity::New);
                self.transfer = None;
            }
            (Page::Identity, "existing") => self.identity = Some(Identity::Existing),
            (Page::Connect, "current")
                if linux && self.destination == Some(Destination::Desktop) =>
            {
                self.transfer = Some(choice.to_string())
            }
            (Page::Connect, "device" | "backup") => self.transfer = Some(choice.to_string()),
            _ => return Err("installer-preview-invalid-choice"),
        }
        Ok(())
    }

    pub fn next(&mut self) -> Result<(), &'static str> {
        if self.destination == Some(Destination::Desktop) && self.page == Page::Destination {
            self.editing = None;
            self.history = vec![Page::Destination];
            self.page = Page::Review;
            return Ok(());
        }
        if self.destination == Some(Destination::Desktop)
            && self.page == Page::Review
            && self.identity.is_none()
        {
            return Err("installer-preview-wrong-page");
        }
        let mut next = match self.page {
            Page::Destination if self.destination.is_some() => match self.destination {
                // The other computer needs its own compatibility step before
                // identity, data or medium choices.
                Some(Destination::Another) => Page::Target,
                _ => Page::Migration,
            },
            Page::Target if self.destination == Some(Destination::Another) => Page::Migration,
            Page::Migration if self.migrate.is_some() => Page::Identity,
            Page::Identity => match self.identity {
                Some(Identity::New) => Page::Profile,
                Some(Identity::Existing) => Page::Connect,
                None => return Err("installer-preview-choose-first"),
            },
            Page::Profile if !self.name.trim().is_empty() => Page::Devices,
            Page::Profile => return Err("installer-preview-name-required"),
            Page::Connect if self.transfer.is_some() => Page::Devices,
            // The destination's normal login password is chosen at first boot.
            // This page only unlocks a checkpoint written by an older build.
            Page::Password => {
                if let Some(error) = self.validation() {
                    return Err(error);
                }
                Page::Devices
            }
            Page::Devices if self.device_name.trim().is_empty() => {
                return Err("installer-preview-device-required");
            }
            Page::Devices
                if self.device_name.len() > 64
                    || self.device_name.chars().any(char::is_control) =>
            {
                return Err("installer-preview-device-invalid");
            }
            // An explicit retained password marks an older prepared handoff.
            // New interactive journeys never set one and skip this question.
            Page::Devices if !self.password.is_empty() => Page::Purpose,
            Page::Purpose | Page::Devices if self.migrate == Some(true) => Page::Belongings,
            Page::Belongings if self.categories[2] => Page::Accounts,
            Page::Devices | Page::Purpose | Page::Belongings | Page::Accounts => Page::Storage,
            Page::Storage => Page::Review,
            Page::Review => {
                self.password.clear();
                self.confirmation.clear();
                Page::Ready
            }
            Page::Ready => return Err("installer-preview-finished"),
            _ => return Err("installer-preview-choose-first"),
        };
        if let Some(edit) = &self.editing {
            next = match (edit.section, self.page) {
                (ReviewSection::Destination, Page::Destination)
                    if self.destination == Some(Destination::Another) =>
                {
                    Page::Target
                }
                (ReviewSection::Destination, Page::Destination | Page::Target) => Page::Storage,
                (ReviewSection::Profile, Page::Profile)
                | (ReviewSection::Password, Page::Password)
                | (ReviewSection::Identity | ReviewSection::Devices, Page::Devices)
                | (ReviewSection::Purpose, Page::Purpose)
                | (ReviewSection::Destination, Page::Storage)
                | (ReviewSection::Belongings, Page::Belongings) => Page::Review,
                (ReviewSection::Belongings, Page::Migration) => {
                    if self.migrate == Some(true) {
                        Page::Belongings
                    } else {
                        Page::Review
                    }
                }
                _ => next,
            };
        }
        if next == Page::Review && self.editing.take().is_some() {
            // A changed branch has a new trail. Back must never visit the
            // old identity or the belongings of a fresh-start branch.
            self.history = vec![Page::Destination, Page::Migration, Page::Identity];
            if self.destination == Some(Destination::Another) {
                self.history.insert(1, Page::Target);
            }
            self.history.push(if self.identity == Some(Identity::New) {
                Page::Profile
            } else {
                Page::Connect
            });
            self.history.push(Page::Devices);
            if !self.password.is_empty() {
                self.history.push(Page::Purpose);
            }
            if self.migrate == Some(true) {
                self.history.push(Page::Belongings);
                if self.categories[2] {
                    self.history.push(Page::Accounts);
                }
            }
            self.history.push(Page::Storage);
            self.page = next;
            return Ok(());
        }
        self.history.push(self.page);
        self.page = next;
        Ok(())
    }

    pub fn toggle_details(&mut self) {
        // Review edits have their own save/cancel trail and restore a snapshot.
        if self.editing.is_some() {
            self.details = !self.details;
            return;
        }
        if self.details {
            if self.history.last() == Some(&Page::Belongings) {
                self.history.pop();
            }
        } else {
            // Overview and details share a page, but are separate navigation stops.
            self.history.push(Page::Belongings);
        }
        self.details = !self.details;
    }

    pub fn back(&mut self) -> Result<(), &'static str> {
        if self.page == Page::Belongings && self.details && self.editing.is_none() {
            self.toggle_details();
            return Ok(());
        }
        if self.cancels_edit() {
            if let Some(edit) = self.editing.take() {
                *self = *edit.original;
            }
            return Ok(());
        }
        let previous = self.history.pop().ok_or("installer-preview-at-start")?;
        self.page = previous;
        self.details =
            previous == Page::Belongings && self.history.last() == Some(&Page::Belongings);
        Ok(())
    }

    pub fn edit(&mut self, section: &str) -> Result<(), &'static str> {
        if self.page != Page::Review {
            return Err("installer-preview-wrong-page");
        }
        if self.destination == Some(Destination::Desktop) && section != "destination" {
            return Err("installer-preview-invalid-choice");
        }
        let (section, start) = match section {
            "destination" => (ReviewSection::Destination, Page::Destination),
            "storage" if self.destination == Some(Destination::Computer) => {
                (ReviewSection::Destination, Page::Storage)
            }
            "profile" if self.identity == Some(Identity::New) => {
                (ReviewSection::Profile, Page::Profile)
            }
            "identity" => (ReviewSection::Identity, Page::Identity),
            "password" => (ReviewSection::Password, Page::Password),
            "devices" => (ReviewSection::Devices, Page::Devices),
            "purpose" if !self.password.is_empty() => (ReviewSection::Purpose, Page::Purpose),
            "belongings" => (ReviewSection::Belongings, Page::Migration),
            "belongings-details" if self.migrate == Some(true) => {
                (ReviewSection::Belongings, Page::Belongings)
            }
            _ => return Err("installer-preview-invalid-choice"),
        };
        self.editing = Some(ReviewEdit {
            section,
            start,
            original: Box::new(self.clone()),
        });
        self.page = start;
        Ok(())
    }

    pub fn cancels_edit(&self) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|edit| self.page == edit.start)
    }

    pub fn editing_profile(&self) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|edit| edit.section == ReviewSection::Profile)
    }

    pub fn editing_destination(&self) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|edit| edit.section == ReviewSection::Destination)
    }

    pub fn editing_belongings(&self) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|edit| edit.section == ReviewSection::Belongings)
    }

    pub fn editing_choices(&self) -> bool {
        self.editing_destination()
            || self.editing_belongings()
            || self.editing.as_ref().is_some_and(|edit| {
                matches!(
                    edit.section,
                    ReviewSection::Devices | ReviewSection::Purpose
                )
            })
    }

    pub fn belongings_source_changed(&self) -> bool {
        self.editing.as_ref().is_some_and(|edit| {
            edit.section == ReviewSection::Belongings
                && (self.migrate != edit.original.migrate
                    || self.categories[..2] != edit.original.categories[..2]
                    || self.browser_domains != edit.original.browser_domains)
        })
    }

    pub fn belongings_accounts_changed(&self) -> Option<bool> {
        self.editing.as_ref().and_then(|edit| {
            if edit.section != ReviewSection::Belongings {
                return None;
            }
            let before = edit.original.migrate == Some(true) && edit.original.categories[2];
            let after = self.migrate == Some(true) && self.categories[2];
            (before != after).then_some(after)
        })
    }

    /// Keep the choices the current edit has made and end it, so navigating
    /// away cannot restore the snapshot the edit started from.
    pub fn keep_edit(&mut self) {
        self.editing = None;
    }

    pub fn destination_edit_changed(&self) -> bool {
        self.editing.as_ref().is_some_and(|edit| {
            edit.section == ReviewSection::Destination
                && self.destination != edit.original.destination
        })
    }

    pub fn saving_edit(&self) -> bool {
        self.editing.as_ref().is_some_and(|edit| {
            (edit.section == ReviewSection::Belongings
                && self.page == Page::Migration
                && self.migrate == Some(false))
                || matches!(
                    (edit.section, self.page),
                    (ReviewSection::Destination, Page::Storage)
                        | (ReviewSection::Profile, Page::Profile)
                        | (
                            ReviewSection::Identity | ReviewSection::Devices,
                            Page::Devices
                        )
                        | (ReviewSection::Purpose, Page::Purpose)
                        | (ReviewSection::Password, Page::Password)
                        | (ReviewSection::Belongings, Page::Belongings)
                )
        })
    }

    pub fn selected_count(&self) -> usize {
        if self.migrate != Some(true) {
            return 0;
        }
        self.categories.iter().filter(|included| **included).count()
    }

    pub fn validation(&self) -> Option<&'static str> {
        match self.page {
            Page::Profile if self.name.trim().is_empty() => Some("installer-preview-name-required"),
            // Only an older password-protected preparation can reach this
            // page. The secret authenticates that retained ciphertext; it is
            // not a new login or disk credential. Its historical policy is
            // retained so those packages remain readable.
            Page::Password => {
                use imager_backend::startup_password::{ValidationError, validate};
                if let Err(reason) = validate(&self.password) {
                    Some(match reason {
                        ValidationError::SurroundingSpaces => "installer-preview-password-spaces",
                        ValidationError::PassphraseTooShort => "installer-preview-password-short",
                        ValidationError::PassphraseCommon => "installer-preview-password-common",
                        ValidationError::NumericPassphraseTooShort => {
                            "installer-preview-password-numeric"
                        }
                        ValidationError::PassphraseRepetitive
                        | ValidationError::PassphraseRepeatedPattern => {
                            "installer-preview-password-repetitive"
                        }
                        ValidationError::Invalid => "installer-preview-password-invalid",
                    })
                } else if self.password != self.confirmation {
                    Some("installer-preview-password-mismatch")
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({
            "preview": true, "page": self.page.key(),
            "destination": self.destination.map(Destination::key), "migrate": self.migrate,
            "identity": self.identity.map(|v| if v == Identity::New { "new" } else { "existing" }),
            "name": self.name, "device_name": self.device_name, "purpose": self.purpose,
            "handoff": if self.password.is_empty() { "deferred" } else { "prepared" },
            "categories": self.categories, "browser_domains": self.browser_domains, "details": self.details,
            "selected_count": self.selected_count(),
            "transfer": self.transfer,
            "editing": self.editing.as_ref().map(|edit| edit.section.key()),
        })
    }
}
