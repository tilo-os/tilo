//! The guided setup's local decisions, with no identity, filesystem or install
//! authority.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Destination,
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
    Method,
    Machine,
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
            Self::Method => "method",
            Self::Machine => "machine",
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
    Machine,
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
            Self::Machine => "machine",
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
    /// The plan another computer is installed from, when a plan is what this
    /// setup makes.
    pub plan: super::machine::Choices,
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
            plan: super::machine::Choices::default(),
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
            plan: super::draft::PlanDraft::from(&self.plan),
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
        self.plan = choices.plan.choices();
        // The plan names the other computer only when that name is its own:
        // an empty name is one the computer answers when it installs.
        if self.destination == Some(Destination::Another) {
            self.plan.name_open = self.device_name.trim().is_empty();
        }
        self.identity = Some(if kind == IdentityKind::New {
            Identity::New
        } else {
            Identity::Existing
        });
        self.transfer = match kind {
            IdentityKind::New => None,
            IdentityKind::Backup | IdentityKind::RetiredEnrolled => Some("backup".into()),
            IdentityKind::CurrentDevice => Some("current".into()),
        };
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
            if self.destination == Some(Destination::Another) && matches!(choice, "os" | "server") {
                self.plan.os = Some(if choice == "os" {
                    super::machine::Os::TiloOs
                } else {
                    super::machine::Os::TiloServer
                });
                if choice == "server" {
                    // A server holds files; it takes no apps or sign-ins from a
                    // desktop, and starts holding nothing until files are chosen.
                    self.categories[1] = false;
                    self.categories[2] = false;
                    if self.migrate.is_none() {
                        self.migrate = Some(false);
                    }
                }
                return Ok(());
            }
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
        if matches!(self.page, Page::Storage | Page::Method | Page::Machine)
            && self.destination != Some(Destination::Another)
        {
            return Err("installer-preview-invalid-choice");
        }
        match (self.page, choice) {
            (Page::Storage, "disk-largest") => {
                self.plan.disk = Some(super::machine::Disks::Largest);
            }
            (Page::Storage, "disk-empty") => self.plan.disk = Some(super::machine::Disks::Empty),
            (Page::Storage, "disk-only") => self.plan.disk = Some(super::machine::Disks::Only),
            (Page::Storage, "disk-open") => self.plan.disk = None,
            (Page::Method, "usb") => self.plan.delivery = super::machine::Delivery::Usb,
            (Page::Method, "netboot") => self.plan.delivery = super::machine::Delivery::Netboot,
            (Page::Method, "alongside") => {
                self.plan.delivery = super::machine::Delivery::Alongside;
            }
            (Page::Machine, "network-dhcp") => {
                self.plan.network = Some(super::machine::Networks::Automatic);
            }
            (Page::Machine, "network-open") => self.plan.network = None,
            (Page::Machine, "unlock-tpm") => {
                self.plan.unlock = Some(super::machine::Unlock::Hardware);
            }
            (Page::Machine, "unlock-owner-device") => {
                self.plan.unlock = Some(super::machine::Unlock::OwnerDevice);
            }
            (Page::Machine, "unlock-passphrase") => {
                self.plan.unlock = Some(super::machine::Unlock::Passphrase);
            }
            (Page::Machine, "unlock-open") => self.plan.unlock = None,
            (Page::Machine, "updates-stable") => {
                self.plan.updates = Some(super::machine::Updates::Stable);
            }
            (Page::Machine, "updates-dev") => {
                self.plan.updates = Some(super::machine::Updates::Development);
            }
            (Page::Machine, "updates-open") => self.plan.updates = None,
            (Page::Machine, "hosting-none") => {
                self.plan.hosting = Some(super::machine::Hosting::HoldsNone);
            }
            (Page::Machine, "hosting-open") => {
                self.plan.hosting = Some(super::machine::Hosting::Open);
            }
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
            (Page::Connect, "backup") => self.transfer = Some(choice.to_string()),
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
            Page::Password => {
                if let Some(error) = self.validation() {
                    return Err(error);
                }
                Page::Devices
            }
            Page::Devices
                if self.device_name.trim().is_empty()
                    && self.destination != Some(Destination::Another) =>
            {
                // A plan may leave the computer's name for the computer to
                // answer when it installs.
                return Err("installer-preview-device-required");
            }
            Page::Devices
                if self.device_name.len() > 64
                    || self.device_name.chars().any(char::is_control) =>
            {
                return Err("installer-preview-device-invalid");
            }
            Page::Devices if !self.password.is_empty() => Page::Purpose,
            Page::Purpose | Page::Devices if self.migrate == Some(true) => Page::Belongings,
            Page::Belongings if self.categories[2] => Page::Accounts,
            Page::Devices | Page::Purpose | Page::Belongings | Page::Accounts => Page::Storage,
            Page::Storage if self.destination == Some(Destination::Another) => Page::Method,
            Page::Method if self.destination == Some(Destination::Another) => Page::Machine,
            Page::Machine if self.destination == Some(Destination::Another) => Page::Review,
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
                (_, Page::Storage)
                    if self.page == Page::Storage
                        && self.destination == Some(Destination::Another) =>
                {
                    Page::Method
                }
                (ReviewSection::Destination, Page::Target) if edit.start == Page::Target => {
                    // The review's model row returns to the review; the
                    // destination it belongs to keeps walking the plan.
                    Page::Review
                }
                (ReviewSection::Destination, Page::Destination | Page::Target) => Page::Storage,
                (ReviewSection::Profile, Page::Profile)
                | (ReviewSection::Password, Page::Password)
                | (ReviewSection::Identity | ReviewSection::Devices, Page::Devices)
                | (ReviewSection::Purpose, Page::Purpose)
                | (ReviewSection::Destination, Page::Storage)
                | (ReviewSection::Machine, Page::Storage | Page::Method | Page::Machine)
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
            if self.destination == Some(Destination::Another) {
                self.history.push(Page::Method);
                self.history.push(Page::Machine);
            }
            self.page = next;
            return Ok(());
        }
        self.history.push(self.page);
        self.page = next;
        Ok(())
    }

    pub fn toggle_details(&mut self) {
        if self.editing.is_some() {
            self.details = !self.details;
            return;
        }
        if self.details {
            if self.history.last() == Some(&Page::Belongings) {
                self.history.pop();
            }
        } else {
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
            "target" if self.destination == Some(Destination::Another) => {
                (ReviewSection::Destination, Page::Target)
            }
            "storage" if self.destination == Some(Destination::Computer) => {
                (ReviewSection::Destination, Page::Storage)
            }
            "storage" if self.destination == Some(Destination::Another) => {
                (ReviewSection::Machine, Page::Storage)
            }
            "machine" if self.destination == Some(Destination::Another) => {
                (ReviewSection::Machine, Page::Machine)
            }
            "delivery" if self.destination == Some(Destination::Another) => {
                (ReviewSection::Machine, Page::Method)
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
                    ReviewSection::Devices | ReviewSection::Purpose | ReviewSection::Machine
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
                        | (
                            ReviewSection::Machine,
                            Page::Storage | Page::Method | Page::Machine
                        )
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
                        ValidationError::PinTooShort
                        | ValidationError::PinNotNumeric
                        | ValidationError::PinPredictable => "installer-preview-password-invalid",
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
            "plan": {
                "os": self.plan.os.map(super::machine::Os::key),
                "name_open": self.plan.name_open,
                "disk": self.plan.disk.map(super::machine::Disks::key),
                "network": self.plan.network.map(super::machine::Networks::key),
                "unlock": self.plan.unlock.map(super::machine::Unlock::key),
                "updates": self.plan.updates.map(super::machine::Updates::key),
                "hosting": self.plan.hosting.map(super::machine::Hosting::key),
                "delivery": self.plan.delivery.key(),
                "open": self.plan.open(),
            },
        })
    }
}
