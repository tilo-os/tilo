//! The capability door: privilege is granted, never inferred.
//!
//! A privileged verb declares the [`Capability`] it needs. Before the app
//! draws a control for that verb it asks for the capability's
//! [`Resolution`], which is one of exactly three answers:
//!
//! | Resolution | What the surface draws |
//! |---|---|
//! | [`Resolution::Granted`] | the real control, doing the real thing |
//! | [`Resolution::Grantable`] | the control plus an explicit grant step, in the person's words |
//! | [`Resolution::Unavailable`] | no control at all: one sentence of why, and what would make it available |
//!
//! A [`Door`] packages the capability with its resolution so a view cannot
//! accidentally draw the granted face for a grantable or unavailable one.
//! [`Door::face`] is the three-way choice; [`Door::control`] is the common
//! "only when granted" shorthand.
//!
//! **No inert control may exist.** A switch, button or menu item for a
//! privileged act is drawn only in the granted face. The grantable face adds
//! the grant step; the unavailable face draws no control and says why.
//!
//! **Discovery is not resolution.** `which()`, `$PATH` and environment
//! variables are test seams (`#[cfg(test)]`, the `testing` module), never the
//! product path. An app may ask the platform or the person for a capability;
//! it may not infer one from its environment.
//!
//! ```
//! use tilo_ui::capability_door::{Capability, Door};
//!
//! let door = Door::grantable(Capability::FIREWALL, "Tilo needs your permission to change firewall rules");
//! let shown: Option<&str> = door.control(|| Some("the Apply button"));
//! assert!(shown.is_none(), "a grantable verb draws no inert control");
//! let grant = door.grant_words().expect("the grant step names itself");
//! assert!(grant.contains("permission"));
//! ```
//!
//! The source of a verdict is the host or the platform:
//! [`Authority::resolve`]. An app whose host gives it a verdict holds a
//! [`Door`]; a capture or test holds one built directly, so neither needs an
//! environment variable to say what it can do.

use std::fmt;

/// A privileged capability an app may be granted, named by the thing it
/// reaches (`network.firewall`, `storage.format`, `session.remote`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Capability(&'static str);

impl Capability {
    /// Change the machine's firewall rules.
    pub const FIREWALL: Self = Self("network.firewall");
    /// Format or write a storage device.
    pub const STORAGE_FORMAT: Self = Self("storage.format");
    /// Write installation media.
    pub const MEDIA_WRITE: Self = Self("media.write");
    /// Install software.
    pub const PACKAGES_INSTALL: Self = Self("packages.install");
    /// Open a remote session.
    pub const SESSION_REMOTE: Self = Self("session.remote");
    /// Restore a backup over existing data.
    pub const BACKUP_RESTORE: Self = Self("backup.restore");

    /// A capability named by its stable dotted id.
    #[must_use]
    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    /// The stable id the platform resolves, and that a grant names.
    #[must_use]
    pub const fn id(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// What the platform says about a capability right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The app holds it; the real control may be drawn.
    Granted,
    /// The app may ask this person for it, once, explicitly. `words` are the
    /// person's words for what is being granted.
    Grantable { words: String },
    /// Nothing the app can do right now reaches the capability. `reason`
    /// says why; `remedy` is what would make it available, when there is
    /// something to do.
    Unavailable {
        reason: String,
        remedy: Option<String>,
    },
}

impl Resolution {
    /// A grant step named in the person's words.
    #[must_use]
    pub fn grantable(words: impl Into<String>) -> Self {
        Self::Grantable {
            words: words.into(),
        }
    }

    /// An unavailable capability with one sentence of why.
    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
            remedy: None,
        }
    }

    /// The same unavailable capability with what would make it available.
    #[must_use]
    pub fn with_remedy(mut self, remedy: impl Into<String>) -> Self {
        if let Self::Unavailable { remedy: at, .. } = &mut self {
            *at = Some(remedy.into());
        }
        self
    }

    /// Whether the real control may be drawn.
    #[must_use]
    pub fn is_granted(&self) -> bool {
        matches!(self, Self::Granted)
    }
}

/// A capability and its resolution: the one thing a view asks before drawing
/// a privileged control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Door {
    capability: Capability,
    resolution: Resolution,
}

impl Door {
    /// The door for `capability` as `resolution` answers it.
    #[must_use]
    pub fn new(capability: Capability, resolution: Resolution) -> Self {
        Self {
            capability,
            resolution,
        }
    }

    /// A capability the app holds.
    #[must_use]
    pub fn granted(capability: Capability) -> Self {
        Self::new(capability, Resolution::Granted)
    }

    /// A capability this person must grant first, in `words`.
    #[must_use]
    pub fn grantable(capability: Capability, words: impl Into<String>) -> Self {
        Self::new(capability, Resolution::grantable(words))
    }

    /// A capability nothing can reach now, with one sentence of why.
    #[must_use]
    pub fn unavailable(capability: Capability, reason: impl Into<String>) -> Self {
        Self::new(capability, Resolution::unavailable(reason))
    }

    /// What capability this door is about.
    #[must_use]
    pub fn capability(&self) -> Capability {
        self.capability
    }

    /// The resolution the platform gave.
    #[must_use]
    pub fn resolution(&self) -> &Resolution {
        &self.resolution
    }

    /// Whether the real control may be drawn and its verb may run.
    #[must_use]
    pub fn is_granted(&self) -> bool {
        self.resolution.is_granted()
    }

    /// Whether the surface must offer the explicit grant step.
    #[must_use]
    pub fn is_grantable(&self) -> bool {
        matches!(self.resolution, Resolution::Grantable { .. })
    }

    /// The grant step's words, when the capability is grantable.
    #[must_use]
    pub fn grant_words(&self) -> Option<&str> {
        match &self.resolution {
            Resolution::Grantable { words } => Some(words),
            _ => None,
        }
    }

    /// Why the capability is unavailable, when it is.
    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&str> {
        match &self.resolution {
            Resolution::Unavailable { reason, .. } => Some(reason),
            _ => None,
        }
    }

    /// What would make the capability available, when the platform names it.
    #[must_use]
    pub fn remedy(&self) -> Option<&str> {
        match &self.resolution {
            Resolution::Unavailable { remedy, .. } => remedy.as_deref(),
            _ => None,
        }
    }

    /// Draw one of the three faces. A view that goes through here cannot
    /// draw the granted control for a capability it does not hold.
    #[must_use]
    pub fn face<T>(
        &self,
        granted: impl FnOnce() -> T,
        grantable: impl FnOnce(&str) -> T,
        unavailable: impl FnOnce(&str, Option<&str>) -> T,
    ) -> T {
        match &self.resolution {
            Resolution::Granted => granted(),
            Resolution::Grantable { words } => grantable(words),
            Resolution::Unavailable { reason, remedy } => unavailable(reason, remedy.as_deref()),
        }
    }

    /// The control when — and only when — the capability is granted. The
    /// grantable and unavailable faces draw no control through this
    /// shorthand; they need their own sentence or grant step, so use
    /// [`face`](Self::face) for them.
    #[must_use]
    pub fn control<T>(&self, build: impl FnOnce() -> T) -> Option<T> {
        self.is_granted().then(build)
    }
}

/// The thing that answers [`Capability`] questions: the host, a portal, or a
/// test fixture. The product path asks this; it never asks the environment.
pub trait Authority {
    /// What `capability` resolves to now.
    fn resolve(&self, capability: Capability) -> Resolution;
}

/// The door `authority` gives `capability`.
#[must_use]
pub fn door(authority: &(impl Authority + ?Sized), capability: Capability) -> Door {
    Door::new(capability, authority.resolve(capability))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Resolution);

    impl Authority for Fixed {
        fn resolve(&self, _capability: Capability) -> Resolution {
            self.0.clone()
        }
    }

    #[test]
    fn each_resolution_draws_its_own_face() {
        // Each face is drawn with `String`s: the three closures answer one
        // `T`, so a borrowed face would tie its closures' lifetimes together.
        let granted = Door::granted(Capability::FIREWALL);
        assert_eq!(granted.control(|| "apply"), Some("apply"));
        assert_eq!(
            granted.face(
                || "control".to_owned(),
                str::to_owned,
                |_, _| "none".to_owned()
            ),
            "control"
        );

        let grantable = Door::grantable(Capability::FIREWALL, "allow changing firewall rules");
        assert_eq!(grantable.control(|| "apply"), None);
        assert_eq!(
            grantable.face(
                || "control".to_owned(),
                str::to_owned,
                |_, _| "none".to_owned()
            ),
            "allow changing firewall rules"
        );

        let unavailable = Door::unavailable(Capability::FIREWALL, "firewalld isn't running").face(
            || "control".to_owned(),
            str::to_owned,
            |reason, remedy| match remedy {
                Some(remedy) => format!("{reason}; {remedy}"),
                None => reason.to_owned(),
            },
        );
        assert_eq!(
            unavailable, "firewalld isn't running",
            "unavailable draws no control, and says why with what would help"
        );
    }

    #[test]
    fn an_authority_answers_the_door_and_carries_the_remedy() {
        let authority = Fixed(Resolution::unavailable("no daemon").with_remedy("start firewalld"));
        let door = door(&authority, Capability::FIREWALL);
        assert_eq!(door.capability(), Capability::FIREWALL);
        assert_eq!(door.unavailable_reason(), Some("no daemon"));
        assert_eq!(door.remedy(), Some("start firewalld"));
        assert!(!door.is_granted());
        assert!(!door.is_grantable());
    }

    #[test]
    fn a_capability_names_the_thing_it_reaches() {
        assert_eq!(Capability::FIREWALL.id(), "network.firewall");
        assert_eq!(Capability::new("custom.thing").to_string(), "custom.thing");
    }
}
