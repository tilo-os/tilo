//! The customization page's fields, over the install profile they edit.

use tilo_ui_base::local_state::LocalState;
use ui::widget::{Dropdown, Field, Form, FormSection, TextInput};

use crate::hosting::Store;
use crate::model::{Filesystem, InstallProfile};
use crate::resource::CATALOG;
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
    fn message(self) -> &'static str {
        match self {
            Self::Hostname => "imager-profile-hostname",
            Self::FullName => "imager-profile-full-name",
            Self::Username => "imager-profile-username",
            Self::Locale => "imager-profile-locale",
            Self::Timezone => "imager-profile-timezone",
            Self::Keyboard => "imager-profile-keyboard",
            Self::Filesystem => "imager-profile-filesystem",
        }
    }

    fn required_message(self) -> &'static str {
        match self {
            Self::Hostname => "imager-profile-hostname-required",
            Self::Username => "imager-profile-username-required",
            _ => "imager-profile-required",
        }
    }

    fn choose_message(self) -> String {
        CATALOG.t_args(
            "imager-profile-choose-listed-field",
            &[("field", tilo_ui_base::i18n::ArgValue::Text(self.label()))],
        )
    }

    pub(crate) fn label(self) -> String {
        CATALOG.t(self.message())
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

    pub(crate) fn id(self) -> String {
        format!("imager.profile.{}", self.key())
    }
}

fn filesystem_message(filesystem: Filesystem) -> &'static str {
    match filesystem {
        Filesystem::Btrfs => "imager-filesystem-btrfs",
        Filesystem::Ext4 => "imager-filesystem-ext4",
        Filesystem::Xfs => "imager-filesystem-xfs",
    }
}

fn shown_value(profile: &InstallProfile, field: CzField) -> String {
    match field {
        CzField::Hostname => profile.hostname.clone(),
        CzField::FullName => profile.user.full_name.clone(),
        CzField::Username => profile.user.username.clone(),
        CzField::Locale => profile.locale.clone(),
        CzField::Timezone => profile.timezone.clone(),
        CzField::Keyboard => keyboard_label(&profile.keyboard).to_string(),
        CzField::Filesystem => CATALOG.t(filesystem_message(profile.filesystem)),
    }
}

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
                    .find(|filesystem| CATALOG.t(filesystem_message(*filesystem)) == value)
            })
            .map(|filesystem| filesystem.key().to_string()),
        CzField::Hostname | CzField::FullName | CzField::Username => Some(value.to_string()),
    }
}

fn text_path(field: CzField) -> Option<tilo_resource::Path<Imager, String>> {
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

pub(crate) fn field_edit(field: CzField, value: String) -> Option<tilo_resource::Edit<Imager>> {
    match text_path(field) {
        Some(path) => Some(path.replace(value)),
        None => Filesystem::from_key(&value)
            .map(|filesystem| Imager::paths().profile.filesystem.set(filesystem)),
    }
}

/// What choosing or typing `shown` into `field` does: edit the profile, or say why it cannot.
fn chosen(field: CzField, shown: &str, store: &Store) {
    let edited = profile_value(field, shown)
        .and_then(|value| field_edit(field, value))
        .ok_or_else(|| field.choose_message())
        .and_then(|edit| store.edit(&edit));
    if let Err(reason) = edited {
        ui::control_notice::say_in_window(&reason);
    }
}

fn text_field(profile: &InstallProfile, field: CzField, store: Option<&Store>) -> Field {
    let input = TextInput::local()
        .with_id(&field.id())
        .with_initial_value(&shown_value(profile, field));
    let input = match store.cloned() {
        Some(store) => input.on_change(move |shown| chosen(field, shown, &store)),
        None => input,
    };
    let control = if field == CzField::Username {
        input.with_kind(ui::widget::TextInputKind::Username)
    } else {
        input
    };
    let required = matches!(field, CzField::Hostname | CzField::Username);
    let row = Field::new(field.label(), control);
    if required && shown_value(profile, field).trim().is_empty() {
        row.with_error(CATALOG.t(field.required_message()))
    } else {
        row
    }
}

fn choice_field(
    profile: &InstallProfile,
    field: CzField,
    options: Vec<String>,
    store: Option<&Store>,
    open: bool,
) -> Field {
    let shown = shown_value(profile, field);
    let listed = options.contains(&shown);
    let state = LocalState::new(shown);
    let mut choice = Dropdown::from_state(options, &state)
        .with_id(&field.id())
        .with_accessible_name(field.label());
    if let Some(store) = store.cloned() {
        choice = choice.on_change(move |shown| chosen(field, shown, &store));
    }
    if open {
        choice = choice.open();
    }
    let row = Field::new(field.label(), choice);
    if listed {
        row
    } else {
        row.with_error(CATALOG.t("imager-profile-choose-listed"))
    }
}

pub(crate) fn form(profile: &InstallProfile, store: Option<&Store>, timezone_open: bool) -> Form {
    let keyboards = KEYBOARDS
        .iter()
        .map(|(label, _)| (*label).to_string())
        .collect();
    let filesystems = Filesystem::all()
        .into_iter()
        .map(|filesystem| CATALOG.t(filesystem_message(filesystem)))
        .collect();
    let locales = install_profile_model::locales()
        .map(str::to_string)
        .collect();
    Form::new()
        .section(
            FormSection::new(CATALOG.t("imager-profile-section-identity"))
                .field(text_field(profile, CzField::Hostname, store))
                .field(text_field(profile, CzField::FullName, store))
                .field(text_field(profile, CzField::Username, store)),
        )
        .section(
            FormSection::new(CATALOG.t("imager-profile-section-region"))
                .field(choice_field(
                    profile,
                    CzField::Timezone,
                    timezones(),
                    store,
                    timezone_open,
                ))
                .field(choice_field(
                    profile,
                    CzField::Locale,
                    locales,
                    store,
                    false,
                ))
                .field(choice_field(
                    profile,
                    CzField::Keyboard,
                    keyboards,
                    store,
                    false,
                )),
        )
        .section(
            FormSection::new(CATALOG.t("imager-profile-section-storage")).field(
                choice_field(profile, CzField::Filesystem, filesystems, store, false)
                    .with_help(crate::setup::strings::t("imager-filesystem-help")),
            ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn customization_catalogs_match_the_embedded_profile_contract() {
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
                profile_value(CzField::Filesystem, "Btrfs"),
                Some("btrfs".into())
            );
            assert_eq!(profile_value(CzField::Filesystem, "FAT32"), None);
        }
    }
}
