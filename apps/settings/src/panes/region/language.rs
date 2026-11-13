//! The person's language: the languages the desktop can show and the language
//! packs this device's repositories offer.
//!
//! One whose pack is installed is chosen at once; one the repositories offer is
//! installed first, through the host's package portal.

use std::sync::Mutex;

use runtime_portal::runtime_request::{Client, OfferedLanguage, Response};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_resource;

use crate::device_read::Reading;
use crate::i18n::t;
use crate::prefs::AppearancePrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;

crate::effects::effect_family!(LanguageEffect => answer);

crate::area::pane_area! {
    LanguageArea;
    fn poll(&self) {
        LanguageArea::poll(self);
    }
}

pub fn area(app: &SettingsApp) -> Option<&LanguageArea> {
    app.areas().find()
}

#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum LanguageEffect {
    /// Show the desktop in a language, installing its language pack first
    /// when the repositories offer one; "device" follows the device's.
    #[effect(idempotent)]
    ChooseLanguage {
        #[param("The language by a tag of it (de, de-DE) or as Region & formats names it")]
        language: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Offered {
    pub languages: Vec<OfferedLanguage>,
    /// Why the host could not say, when it could not.
    pub unavailable: Option<String>,
}

pub struct LanguageArea {
    pub offered: Reading<Offered>,
    pub installing: LocalState<Option<String>>,
    /// Why the last install failed.
    pub failed: LocalState<Option<String>>,
    pub shown: LocalState<u64>,
    seen: std::cell::Cell<(u64, u64, u64)>,
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
            shown: LocalState::new(0),
            seen: std::cell::Cell::new((0, 0, 0)),
        }
    }

    fn poll(&self) {
        let now = (
            self.offered.revision(),
            self.installing.revision(),
            self.failed.revision(),
        );
        if self.seen.replace(now) != now {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    /// Ask the host what it offers unless it was asked already.
    pub fn read(&self) {
        if self.offered.get().is_none() {
            self.offered.refresh();
        }
    }
}

#[must_use]
pub fn choices(installed: &[&str], offered: &Offered) -> Vec<String> {
    let shown = installed.iter().copied();
    let offered = offered
        .languages
        .iter()
        .map(|offered| offered.locale.as_str());
    let mut choices: Vec<String> = Vec::new();
    for tag in shown.chain(offered) {
        let language = tilo_paths::language_of(tag);
        if !choices.contains(&language) {
            choices.push(language);
        }
    }
    choices
}

pub const DEVICE: &str = "device";

pub(crate) fn value(app: &SettingsApp, follows: &crate::setting::Follows) -> String {
    name(
        follows
            .get::<AppearancePrefs>(app.storage())
            .language
            .as_deref(),
    )
}

#[must_use]
pub fn name(language: Option<&str>) -> String {
    language.map_or_else(
        || t("settings-region-language-device"),
        ui::i18n::language_name,
    )
}

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
                tilo_rt::sleep_thread(std::time::Duration::from_millis(250))
                    .map_err(|error| error.to_string())?;
            }
            Response::Failed { message } | Response::Refused { message } => return Err(message),
            other @ Response::LanguagePacks { .. } => {
                return Err(format!("the installer answered {other:?}"));
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Chosen {
    Device,
    Language(String),
}

fn resolve(language: &str, choices: &[String]) -> Option<Chosen> {
    if language == DEVICE || language == name(None) {
        return Some(Chosen::Device);
    }
    let language_of = || tilo_paths::language_of(language);
    choices
        .iter()
        .find(|choice| {
            choice.eq_ignore_ascii_case(language)
                || name(Some(choice)) == language
                || **choice == language_of()
        })
        .map(|choice| Chosen::Language(choice.clone()))
}

fn installed(language: &str) -> bool {
    let language = tilo_paths::language_of(language);
    crate::i18n::CATALOG
        .languages()
        .iter()
        .any(|tag| tilo_paths::language_of(tag) == language)
}

pub(crate) fn answer(
    app: &SettingsApp,
    effect: LanguageEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let LanguageEffect::ChooseLanguage { language } = effect;
    let languages = area(app).ok_or_else(crate::area::missing)?;
    let offered = languages.offered.get().unwrap_or_default();
    let chosen = resolve(
        &language,
        &choices(&crate::i18n::CATALOG.languages(), &offered),
    )
    .ok_or_else(|| format!("`{language}` is not a language this desktop has or offers"))?;
    languages.failed.set(None);
    let locale = match chosen {
        Chosen::Language(language) if !installed(&language) => language,
        here => {
            let language = match here {
                Chosen::Device => None,
                Chosen::Language(language) => Some(language),
            };
            return app
                .sections()
                .invoke::<AppearancePrefs>(AppearancePrefs::paths().language.set(language))
                .map(|()| Effected::done());
        }
    };
    languages.installing.set(Some(locale.clone()));
    let chosen = locale.clone();
    Ok(app.job(
        cx,
        move || install(&locale),
        move |app: &SettingsApp, outcome: Result<(), String>, _| {
            let area = area(app).ok_or_else(crate::area::missing)?;
            area.installing.set(None);
            match outcome {
                Ok(()) => {
                    area.offered.refresh();
                    ui::i18n::refresh_language_packs();
                    app.sections()
                        .invoke::<AppearancePrefs>(
                            AppearancePrefs::paths().language.set(Some(chosen)),
                        )
                        .map(|()| Effected::done())
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

    #[test]
    fn german_is_one_choice_named_as_its_pack_is() {
        let offered = Offered {
            languages: ["de", "fr"]
                .map(|locale| OfferedLanguage {
                    locale: locale.into(),
                    installed: false,
                })
                .to_vec(),
            unavailable: None,
        };
        assert_eq!(choices(&["en-US", "de-DE"], &offered), ["en", "de", "fr"]);
    }
}
