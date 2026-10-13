//! The **guided-customization form** — the one composition that collects a
//! Tilo install profile (identity, region, storage) for *both* imager flows:
//! writing a pre-configured image to a stick, and installing Tilo onto a disk.
//!
//! Every field is a toolkit control in a [`Form`]: names are [`TextInput`]s,
//! the catalog choices are [`Dropdown`]s that filter while open. Each edit is
//! reported to the wizard as a [`Command::Profile`], which submits the field
//! edit at the profile's path — one undo boundary, replicated to
//! collaborators and the same edit the palette or an agent would make. The form
//! holds no profile of its own: it is composed from an [`InstallProfile`]
//! snapshot, so the document stays authoritative. Secrets stay off the host
//! and are collected only on the target machine (HIG §3).

use std::rc::Rc;

use ui::local_state::LocalState;
use ui::widget::{Dropdown, Field, Form, FormSection, TextInput};

use crate::app::{Command, Commands};
use crate::model::{Filesystem, InstallProfile};
use crate::resource::Imager;

const KEYBOARDS: &[(&str, &str)] = &[
    ("English (US)", "us"),
    ("English (UK)", "gb"),
    ("German (QWERTZ)", "de"),
    ("French (AZERTY)", "fr"),
    ("Spanish", "es"),
    ("Italian", "it"),
    ("Portuguese", "pt"),
    ("Portuguese (Brazil)", "br"),
    ("Russian (ЙЦУКЕН)", "ru"),
    ("Swedish", "se"),
    ("Norwegian", "no"),
    ("Danish", "dk"),
    ("Swiss", "ch"),
    ("Polish", "pl"),
    ("Dvorak", "dvorak"),
    ("Colemak", "colemak"),
];

const IANA_TIMEZONES: &str = include_str!("iana_timezones.txt");

fn keyboard_label(id: &str) -> &str {
    KEYBOARDS
        .iter()
        .find(|(_, candidate)| *candidate == id)
        .map_or(id, |(label, _)| *label)
}

fn keyboard_id(label: &str) -> Option<&'static str> {
    KEYBOARDS
        .iter()
        .find(|(candidate, _)| *candidate == label)
        .map(|(_, id)| *id)
}

fn timezones() -> Vec<String> {
    // A comprehensive canonical catalog is compiled into every build. Runtime
    // discovery does not determine whether Windows/macOS users get a worldwide
    // chooser, and cannot introduce host-only aliases that the live image may
    // reject at its zoneinfo trust boundary.
    let mut zones = IANA_TIMEZONES
        .lines()
        .map(str::trim)
        .filter(|zone| !zone.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    zones.sort();
    zones.dedup();
    zones
}

/// One editable field of the profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CzField {
    Hostname,
    FullName,
    Username,
    Locale,
    Timezone,
    Keyboard,
    Filesystem,
}

impl CzField {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Hostname => "Hostname",
            Self::FullName => "Full name",
            Self::Username => "Username",
            Self::Locale => "Language and locale",
            Self::Timezone => "Time zone",
            Self::Keyboard => "Keyboard layout",
            Self::Filesystem => "Filesystem",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Hostname => "hostname",
            Self::FullName => "full-name",
            Self::Username => "username",
            Self::Locale => "locale",
            Self::Timezone => "timezone",
            Self::Keyboard => "keyboard",
            Self::Filesystem => "filesystem",
        }
    }

    /// The control's id on the page.
    pub(crate) fn id(self) -> String {
        format!("imager.profile.{}", self.key())
    }
}

/// The value `field` holds in `profile`, as the form shows it.
fn shown_value(profile: &InstallProfile, field: CzField) -> String {
    match field {
        CzField::Hostname => profile.hostname.clone(),
        CzField::FullName => profile.user.full_name.clone(),
        CzField::Username => profile.user.username.clone(),
        CzField::Locale => profile.locale.clone(),
        CzField::Timezone => profile.timezone.clone(),
        CzField::Keyboard => keyboard_label(&profile.keyboard).to_string(),
        CzField::Filesystem => profile.filesystem.label().to_string(),
    }
}

/// The profile value for what the person chose or typed in `field`,
/// normalized through the same catalogs the choosers show. `None` for a
/// value no catalog lists. Free-text identity fields stay as typed.
pub(crate) fn profile_value(field: CzField, value: &str) -> Option<String> {
    match field {
        CzField::Locale => install_profile_model::locales()
            .any(|locale| locale == value)
            .then(|| value.to_string()),
        CzField::Timezone => timezones().into_iter().find(|zone| zone == value),
        CzField::Keyboard => keyboard_id(value)
            .or_else(|| {
                KEYBOARDS
                    .iter()
                    .find(|(_, id)| *id == value)
                    .map(|(_, id)| *id)
            })
            .map(str::to_string),
        CzField::Filesystem => Filesystem::from_key(&value.to_ascii_lowercase())
            .or_else(|| {
                Filesystem::all()
                    .into_iter()
                    .find(|filesystem| filesystem.label() == value)
            })
            .map(|filesystem| filesystem.key().to_string()),
        CzField::Hostname | CzField::FullName | CzField::Username => Some(value.to_string()),
    }
}

/// The text path of `field` in the profile; `None` for the filesystem,
/// which is a choice.
fn text_path(field: CzField) -> Option<ui::tilo_resource::Path<Imager, String>> {
    let profile = Imager::paths().profile;
    Some(match field {
        CzField::Hostname => profile.hostname,
        CzField::FullName => profile.user.full_name,
        CzField::Username => profile.user.username,
        CzField::Locale => profile.locale,
        CzField::Timezone => profile.timezone,
        CzField::Keyboard => profile.keyboard,
        CzField::Filesystem => return None,
    })
}

/// The field edit that sets `field` to the normalized `value`; `None` for
/// a filesystem the installer does not know.
pub(crate) fn field_edit(field: CzField, value: String) -> Option<ui::tilo_resource::Edit<Imager>> {
    match text_path(field) {
        Some(path) => Some(path.replace(value)),
        None => Filesystem::from_key(&value)
            .map(|filesystem| Imager::paths().profile.filesystem.set(filesystem)),
    }
}

fn text_field(
    profile: &InstallProfile,
    field: CzField,
    commands: &Commands,
    actions: Option<&ui::framework::Actions<Imager>>,
) -> Field {
    let sink = Rc::clone(commands);
    let input = TextInput::local()
        .with_id(&field.id())
        .with_initial_value(&shown_value(profile, field));
    let input = if let Some(binding) = actions.zip(text_path(field)).and_then(|(actions, path)| {
        actions.edit_binding(path, |path, value: String| path.replace(value))
    }) {
        input.on_action(binding)
    } else {
        input.on_change(move |value| {
            sink.borrow_mut()
                .push(Command::Profile(field, value.to_string()));
        })
    };
    let control = if field == CzField::Username {
        input.with_kind(ui::widget::TextInputKind::Username)
    } else {
        input
    };
    let required = matches!(field, CzField::Hostname | CzField::Username);
    let row = Field::new(field.label(), control);
    if required && shown_value(profile, field).trim().is_empty() {
        row.with_error(format!("Enter a {}.", field.label().to_lowercase()))
    } else {
        row
    }
}

fn choice_field(
    profile: &InstallProfile,
    field: CzField,
    options: Vec<String>,
    commands: &Commands,
    open: bool,
) -> Field {
    let shown = shown_value(profile, field);
    let listed = options.contains(&shown);
    let sink = Rc::clone(commands);
    let state = LocalState::new(shown);
    let mut choice = Dropdown::from_state(options, &state)
        .with_id(&field.id())
        .with_accessible_name(field.label())
        .on_change(move |value| {
            sink.borrow_mut()
                .push(Command::Profile(field, value.to_string()));
        });
    if open {
        choice = choice.open();
    }
    let row = Field::new(field.label(), choice);
    if listed {
        row
    } else {
        row.with_error("Choose a value from the list.")
    }
}

/// The customization form for `profile`. `timezone_open` poses the
/// time-zone chooser open for a documentation frame.
pub(crate) fn form(
    profile: &InstallProfile,
    commands: &Commands,
    actions: Option<&ui::framework::Actions<Imager>>,
    timezone_open: bool,
) -> Form {
    let keyboards = KEYBOARDS
        .iter()
        .map(|(label, _)| (*label).to_string())
        .collect();
    let filesystems = Filesystem::all()
        .into_iter()
        .map(|filesystem| filesystem.label().to_string())
        .collect();
    let locales = install_profile_model::locales()
        .map(str::to_string)
        .collect();
    Form::new()
        .section(
            FormSection::new("Identity")
                .field(text_field(profile, CzField::Hostname, commands, actions))
                .field(text_field(profile, CzField::FullName, commands, actions))
                .field(text_field(profile, CzField::Username, commands, actions)),
        )
        .section(
            FormSection::new("Region")
                .field(choice_field(
                    profile,
                    CzField::Timezone,
                    timezones(),
                    commands,
                    timezone_open,
                ))
                .field(choice_field(
                    profile,
                    CzField::Locale,
                    locales,
                    commands,
                    false,
                ))
                .field(choice_field(
                    profile,
                    CzField::Keyboard,
                    keyboards,
                    commands,
                    false,
                )),
        )
        // Security, the storage target and destructive choices are completed
        // on the device; the filesystem is the one storage preference here.
        .section(FormSection::new("Storage").field(choice_field(
            profile,
            CzField::Filesystem,
            filesystems,
            commands,
            false,
        )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Timezone catalog and canonical values.
    #[test]
    fn customization_catalogs_match_the_embedded_profile_contract() {
        // Phase `built_in_timezone_catalog_is_worldwide_canonical_and_safe`.
        {
            let zones = timezones();
            assert!(zones.len() >= 400, "catalog unexpectedly incomplete");
            for representative in [
                "Africa/Nairobi",
                "America/New_York",
                "Antarctica/McMurdo",
                "Asia/Kolkata",
                "Atlantic/Reykjavik",
                "Australia/Sydney",
                "Europe/Berlin",
                "Pacific/Auckland",
                "UTC",
            ] {
                assert!(zones.iter().any(|zone| zone == representative));
            }
            assert!(zones.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(zones.iter().all(|zone| {
                !zone.starts_with('/')
                    && !zone.contains("..")
                    && zone.bytes().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, b'_' | b'/' | b'+' | b'-')
                    })
            }));
        }

        // Phase `canonical_values_match_the_embedded_profile_contract`.
        {
            assert_eq!(
                profile_value(CzField::Keyboard, "German (QWERTZ)"),
                Some("de".into())
            );
            assert_eq!(profile_value(CzField::Keyboard, "de"), Some("de".into()));
            assert_eq!(
                profile_value(CzField::Timezone, "Europe/Berlin"),
                Some("Europe/Berlin".into())
            );
            assert_eq!(
                profile_value(CzField::Timezone, "Europe/DefinitelyMissing"),
                None
            );
            assert_eq!(profile_value(CzField::Locale, "not a locale"), None);
            assert_eq!(
                profile_value(CzField::Filesystem, "xfs"),
                Some("xfs".into())
            );
            assert_eq!(
                profile_value(CzField::Filesystem, "Btrfs (recommended)"),
                Some("btrfs".into())
            );
            assert_eq!(profile_value(CzField::Filesystem, "FAT32"), None);
        }
    }
}
