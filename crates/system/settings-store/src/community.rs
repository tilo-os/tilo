//! Whether this person is in the Tilo Community.
//!
//! Everyone is a member by default: the shell joins the community room once
//! the session has an identity and the network is up. Leaving is the one
//! choice a person makes here, and it is theirs, not the device's, so
//! `tilo.community.membership` is personal and follows them to every device.
//! A device that learns `left` leaves the room and never joins it again on
//! its own; only "Join the community" sets it back to `auto`.
//!
//! `tilo.community.refused_invite` is what the device observed, not a
//! choice: the id (hex) of the standing invitation the community's host
//! refused. That invitation is not redeemed again; a different one, shipped
//! by a later release, is.

use std::path::Path;

use serde_json::{Map, Value};

/// The settings section.
pub const SECTION: &str = "community";
/// The person's membership choice: [`Membership`].
pub const MEMBERSHIP: &str = "membership";
/// The standing invitation the community's host refused, as hex.
pub const REFUSED_INVITE: &str = "refused_invite";

/// The person's membership choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Membership {
    /// In the community, joined automatically on every device.
    #[default]
    Auto,
    /// Left it; never joined again automatically.
    Left,
}

impl Membership {
    /// The value written to the settings document.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Left => "left",
        }
    }

    /// Read a stored value. Anything but `left` is the default: a person is
    /// only out of the community when they said so.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("left") => Self::Left,
            _ => Self::Auto,
        }
    }
}

/// The membership choice in a loaded settings document.
#[must_use]
pub fn membership_in(doc: &Map<String, Value>) -> Membership {
    Membership::parse(crate::str_at(doc, SECTION, MEMBERSHIP))
}

/// The refused invitation id in a loaded settings document.
#[must_use]
pub fn refused_invite_in(doc: &Map<String, Value>) -> Option<&str> {
    crate::str_at(doc, SECTION, REFUSED_INVITE).filter(|id| !id.is_empty())
}

/// Remember the membership choice in the settings file.
///
/// # Errors
///
/// No config directory, or the file cannot be written.
pub fn set_membership(membership: Membership) -> std::io::Result<()> {
    crate::update_section(SECTION, membership_values(membership))
}

/// [`set_membership`] against an explicit settings file.
///
/// # Errors
///
/// The file cannot be written.
pub fn set_membership_at(path: &Path, membership: Membership) -> std::io::Result<()> {
    crate::update_section_at(path, SECTION, membership_values(membership))
}

/// Remember the refused invitation, or forget it with `None`.
///
/// # Errors
///
/// No config directory, or the file cannot be written.
pub fn set_refused_invite(invite_id: Option<&str>) -> std::io::Result<()> {
    let mut values = Map::new();
    values.insert(
        REFUSED_INVITE.into(),
        invite_id.map_or(Value::Null, |id| Value::String(id.to_string())),
    );
    crate::update_section(SECTION, values)
}

/// Choosing to be in the community also asks once more after a refusal.
fn membership_values(membership: Membership) -> Map<String, Value> {
    let mut values = Map::new();
    values.insert(MEMBERSHIP.into(), Value::String(membership.as_str().into()));
    if membership == Membership::Auto {
        values.insert(REFUSED_INVITE.into(), Value::Null);
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_is_in_unless_they_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(membership_in(&crate::load_at(&path)), Membership::Auto);
        set_membership_at(&path, Membership::Left).unwrap();
        assert_eq!(membership_in(&crate::load_at(&path)), Membership::Left);
        set_membership_at(&path, Membership::Auto).unwrap();
        assert_eq!(membership_in(&crate::load_at(&path)), Membership::Auto);
        assert_eq!(Membership::parse(Some("someday")), Membership::Auto);
        assert!(crate::continuity::is_personal("tilo.community.membership"));
        assert_eq!(
            crate::continuity::classify("tilo.community.refused_invite"),
            Some(crate::continuity::SettingScope::Derived)
        );
    }
}
