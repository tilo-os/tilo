//! The guided setup's local decisions, with no identity, filesystem or install
//! authority.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Destination,
    Target,
    Migrate,
    Password,
    Devices,
    Purpose,
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
            Self::Migrate => "migrate",
            Self::Password => "password",
            Self::Devices => "devices",
            Self::Purpose => "purpose",
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
pub enum ReviewSection {
    Destination,
    Password,
    Devices,
    Purpose,
    Machine,
}

impl ReviewSection {
    pub fn key(self) -> &'static str {
        match self {
            Self::Destination => "destination",
            Self::Password => "password",
            Self::Devices => "devices",
            Self::Purpose => "purpose",
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
    pub device_name: String,
    pub purpose: String,
    pub password: String,
    pub confirmation: String,
    pub history: Vec<Page>,
    editing: Option<ReviewEdit>,
}

impl Journey {
    pub fn new() -> Self {
        Self {
            page: Page::Destination,
            destination: None,
            plan: super::machine::Choices::default(),
            device_name: String::new(),
            purpose: install_profile_model::default_purpose(),
            password: String::new(),
            confirmation: String::new(),
            history: Vec::new(),
            editing: None,
        }
    }

    fn reset_for_destination(&mut self, destination: Destination) {
        *self = Self::new();
        self.destination = Some(destination);
    }

    pub fn choose(&mut self, choice: &str, linux: bool) -> Result<(), &'static str> {
        if self.page == Page::Destination {
            if self.destination == Some(Destination::Another) && matches!(choice, "os" | "server") {
                self.plan.os = Some(if choice == "os" {
                    super::machine::Os::TiloOs
                } else {
                    super::machine::Os::TiloServer
                });
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
        let mut next = match self.page {
            Page::Destination if self.destination.is_some() => match self.destination {
                Some(Destination::Another) => Page::Target,
                _ => Page::Migrate,
            },
            Page::Target if self.destination == Some(Destination::Another) => Page::Migrate,
            // Bringing what this computer holds is optional: a person may
            // start with nothing.
            Page::Migrate => Page::Devices,
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
            Page::Devices | Page::Purpose => Page::Storage,
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
                (ReviewSection::Password, Page::Password)
                | (ReviewSection::Devices, Page::Devices)
                | (ReviewSection::Purpose, Page::Purpose)
                | (ReviewSection::Destination, Page::Storage)
                | (ReviewSection::Machine, Page::Storage | Page::Method | Page::Machine) => {
                    Page::Review
                }
                _ => next,
            };
        }
        if next == Page::Review && self.editing.take().is_some() {
            self.history = vec![Page::Destination, Page::Migrate];
            if self.destination == Some(Destination::Another) {
                self.history.insert(1, Page::Target);
            }
            self.history.push(Page::Devices);
            if !self.password.is_empty() {
                self.history.push(Page::Purpose);
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

    pub fn back(&mut self) -> Result<(), &'static str> {
        if self.cancels_edit() {
            if let Some(edit) = self.editing.take() {
                *self = *edit.original;
            }
            return Ok(());
        }
        self.page = self.history.pop().ok_or("installer-preview-at-start")?;
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
            "password" => (ReviewSection::Password, Page::Password),
            "devices" => (ReviewSection::Devices, Page::Devices),
            "purpose" if !self.password.is_empty() => (ReviewSection::Purpose, Page::Purpose),
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

    pub fn editing_destination(&self) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|edit| edit.section == ReviewSection::Destination)
    }

    pub fn editing_choices(&self) -> bool {
        self.editing_destination()
            || self.editing.as_ref().is_some_and(|edit| {
                matches!(
                    edit.section,
                    ReviewSection::Devices | ReviewSection::Purpose | ReviewSection::Machine
                )
            })
    }

    pub fn saving_edit(&self) -> bool {
        self.editing.as_ref().is_some_and(|edit| {
            matches!(
                (edit.section, self.page),
                (ReviewSection::Destination, Page::Storage)
                    | (
                        ReviewSection::Machine,
                        Page::Storage | Page::Method | Page::Machine
                    )
                    | (ReviewSection::Devices, Page::Devices)
                    | (ReviewSection::Purpose, Page::Purpose)
                    | (ReviewSection::Password, Page::Password)
            )
        })
    }

    pub fn validation(&self) -> Option<&'static str> {
        match self.page {
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
            "destination": self.destination.map(Destination::key),
            "device_name": self.device_name, "purpose": self.purpose,
            "handoff": if self.password.is_empty() { "deferred" } else { "prepared" },
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
