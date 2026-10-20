//! The person's language: the languages the desktop has, the ones the
//! device's repositories offer a language pack for, and choosing one.
//!
//! A language whose pack is installed is chosen at once
//! (`appearance.language`, which every app and the shell follow). One the
//! repositories offer is installed first, by the installer service the
//! launcher hands Settings for that alone
//! (`runtime_portal::runtime_request`), and chosen when its pack has
//! landed, so every app finds its catalogues when it switches.

use std::sync::Mutex;

use runtime_portal::runtime_request::{Client, OfferedLanguage, Response};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx};
use ui::tilo_resource;

use crate::device_read::Reading;
use crate::i18n::t;
use crate::prefs::AppearancePrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;

crate::effects::effect_family!(LanguageEffect => answer);

crate::area::pane_area! { LanguageArea; }

/// The window's [`LanguageArea`].
pub fn area(app: &SettingsApp) -> Option<&LanguageArea> {
    app.areas().find()
}

/// Choosing the person's language.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum LanguageEffect {
    /// Show the desktop in a language, installing its language pack first
    /// when the repositories offer one; "device" follows the device's.
    #[effect(idempotent)]
    ChooseLanguage {
        #[param("The language as a tag (de-DE) or as Region & formats names it")]
        language: String,
    },
}

/// The languages the device's repositories offer a pack for, as the host
/// last answered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Offered {
    pub languages: Vec<OfferedLanguage>,
    /// Why the host could not say, when it could not.
    pub unavailable: Option<String>,
}

/// What [`LanguageArea`] knows of the person's languages.
pub struct LanguageArea {
    /// What the repositories offer, read when Region & formats opens.
    pub offered: Reading<Offered>,
    /// The language whose pack is being installed.
    pub installing: LocalState<Option<String>>,
    /// Why the last install failed.
    pub failed: LocalState<Option<String>>,
}

impl Default for LanguageArea {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageArea {
    /// Nothing is read until the page opens.
    #[must_use]
    pub fn new() -> Self {
        Self {
            offered: Reading::new(read_offered),
            installing: LocalState::new(None),
            failed: LocalState::new(None),
        }
    }

    /// Ask the host what it offers unless it was asked already.
    pub fn read(&self) {
        if self.offered.get().is_none() {
            self.offered.refresh();
        }
    }

    /// Every language a person can choose here.
    #[must_use]
    pub fn choices(&self) -> Vec<String> {
        choices(&self.offered.get().unwrap_or_default())
    }
}

/// Every language a person can choose: the installed ones, then those the
/// repositories offer, each once.
#[must_use]
pub fn choices(offered: &Offered) -> Vec<String> {
    let mut choices: Vec<String> = crate::i18n::languages()
        .into_iter()
        .map(str::to_owned)
        .collect();
    for offered in &offered.languages {
        if !choices.contains(&offered.locale) {
            choices.push(offered.locale.clone());
        }
    }
    choices
}

/// The value that follows the device's language.
pub const DEVICE: &str = "device";

/// `language` as Region & formats names it.
#[must_use]
pub fn name(language: Option<&str>) -> String {
    language.map_or_else(
        || t("settings-region-language-device"),
        ui::i18n::language_name,
    )
}

/// The connection to the installer service the launcher handed Settings.
fn with_host<T>(ask: impl FnOnce(&mut Client) -> std::io::Result<T>) -> Result<T, String> {
    static HOST: std::sync::OnceLock<Option<Mutex<Client>>> = std::sync::OnceLock::new();
    let host = HOST
        .get_or_init(|| Client::from_service().map(Mutex::new))
        .as_ref()
        .ok_or_else(|| t("settings-region-language-no-installer"))?;
    let mut client = host
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let answer = ask(&mut client);
    drop(client);
    answer.map_err(|error| error.to_string())
}

fn read_offered() -> Offered {
    match with_host(Client::language_packs) {
        Ok(Response::LanguagePacks { offered }) => Offered {
            languages: offered,
            unavailable: None,
        },
        Ok(Response::Failed { message } | Response::Refused { message }) | Err(message) => {
            Offered {
                languages: Vec::new(),
                unavailable: Some(message),
            }
        }
        Ok(other) => Offered {
            languages: Vec::new(),
            unavailable: Some(format!("the installer answered {other:?}")),
        },
    }
}

/// Install the pack of `locale`, following the install until it lands or
/// stops. Off the UI thread.
fn install(locale: &str) -> Result<(), String> {
    loop {
        match with_host(|host| host.install_language_pack(locale))? {
            Response::Installed => return Ok(()),
            Response::Installing { .. } => {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Response::Failed { message } | Response::Refused { message } => return Err(message),
            other => return Err(format!("the installer answered {other:?}")),
        }
    }
}

/// What a person chose.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Chosen {
    /// The device's own language.
    Device,
    /// A language, by its tag.
    Language(String),
}

/// What `language` names: a tag among `choices`, a choice's name, or the
/// device's own.
fn resolve(language: &str, choices: &[String]) -> Option<Chosen> {
    if language == DEVICE || language == name(None) {
        return Some(Chosen::Device);
    }
    choices
        .iter()
        .find(|choice| choice.eq_ignore_ascii_case(language) || name(Some(choice)) == language)
        .map(|choice| Chosen::Language(choice.clone()))
}

/// Answer `effect`: choose an installed language at once, or install an
/// offered one first.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: LanguageEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let LanguageEffect::ChooseLanguage { language } = effect;
    let languages = area(app).ok_or_else(crate::area::missing)?;
    let chosen = resolve(&language, &languages.choices())
        .ok_or_else(|| format!("`{language}` is not a language this desktop has or offers"))?;
    let locale = match chosen {
        Chosen::Language(locale) if !crate::i18n::languages().contains(&locale.as_str()) => locale,
        installed => {
            languages.failed.set(None);
            let language = match installed {
                Chosen::Device => None,
                Chosen::Language(locale) => Some(locale),
            };
            return app
                .host()
                .update::<AppearancePrefs>(move |prefs| prefs.language = language);
        }
    };
    languages.failed.set(None);
    languages.installing.set(Some(locale.clone()));
    let chosen = locale.clone();
    Ok(crate::device_read::job(
        cx,
        move || install(&locale),
        move |app: &mut SettingsApp, outcome: Result<(), String>, _| {
            let area = area(app).ok_or_else(crate::area::missing)?;
            area.installing.set(None);
            match outcome {
                Ok(()) => {
                    area.offered.refresh();
                    // The pack is on disk: this window reads it too.
                    ui::i18n::refresh_language_packs();
                    app.host()
                        .update::<AppearancePrefs>(move |prefs| prefs.language = Some(chosen))
                }
                Err(error) => {
                    area.failed.set(Some(error.clone()));
                    Err(error)
                }
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A language is named by its tag, by the name the page shows, or as
    /// the device's own; anything else is not a language here.
    #[test]
    fn a_language_is_chosen_by_its_tag_or_its_name() {
        let choices = vec!["en-US".to_owned(), "de-DE".to_owned()];
        let german = Some(Chosen::Language("de-DE".into()));
        assert_eq!(resolve("de-DE", &choices), german);
        assert_eq!(resolve("de-de", &choices), german);
        assert_eq!(resolve("Deutsch (Deutschland)", &choices), german);
        assert_eq!(resolve(DEVICE, &choices), Some(Chosen::Device));
        assert_eq!(resolve("xx-XX", &choices), None);
    }
}
