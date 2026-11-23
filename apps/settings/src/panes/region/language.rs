//! The person's language: the languages the desktop can show and the language packs the
//! workspace's registries offer.
//!
//! One whose pack this device has is chosen at once. One a registry offers is first named in the
//! workspace's tree (`workspace/apps#install`, the same action every app installs through), and
//! the device's daemon installs it: the pane follows the call and switches the desktop once it
//! lands.

use std::cell::{Cell, RefCell};

use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_actions::remote::{Confirmation, InvokeArgument, workspace_governance};
use ui::tilo_resource;
use ui::{Arrival, InvocationCall, LocalState, RemoteActions};

use crate::i18n::t;
use crate::prefs::AppearancePrefs;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;

crate::effects::effect_family!(LanguageEffect => answer);

crate::area::pane_area! {
    LanguageArea;
    crate::area::serves!(RegionFormats);
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
    /// Show the desktop in a language, installing its language pack first when the workspace's
    /// registries offer one; "device" follows the device's.
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    ChooseLanguage {
        #[param("The language by a tag of it (de, de-DE) or as Region & formats names it")]
        language: String,
    },
}

/// One language a registry offers, as `workspace/apps#offers` names it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct LanguageOffer {
    /// The workspace's record id for the pack (`so.tilo.language-pack.de`).
    pub app: String,
    /// The language it is for (`de`).
    #[serde(skip)]
    pub locale: String,
    pub version: String,
    /// The signed package root, which the install names.
    pub package: String,
    /// The key that signed the package.
    pub publisher: String,
    /// The surface the version publishes; empty for a language pack.
    pub surface: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Offered {
    pub languages: Vec<LanguageOffer>,
    /// Why the workspace could not say, when it could not.
    pub unavailable: Option<String>,
}

/// What a read of the offers answered: the offers the registries make, each pack's language read
/// from its record id.
fn offers_from(answer: serde_json::Value) -> Result<Vec<LanguageOffer>, String> {
    #[derive(serde::Deserialize)]
    struct Answer {
        #[serde(default)]
        offers: Vec<RawOffer>,
    }
    #[derive(serde::Deserialize)]
    struct RawOffer {
        app: String,
        version: String,
        package: String,
        publisher: String,
        #[serde(default)]
        surface: String,
    }
    let answer: Answer =
        serde_json::from_value(answer).map_err(|error| format!("the offers: {error}"))?;
    let mut offers: Vec<LanguageOffer> = answer
        .offers
        .into_iter()
        .filter_map(|offer| {
            let locale =
                tilo_app_package::language_pack_locale(&offer.app).map(tilo_paths::language_of)?;
            Some(LanguageOffer {
                app: offer.app,
                locale,
                version: offer.version,
                package: offer.package,
                publisher: offer.publisher,
                surface: offer.surface,
            })
        })
        .collect();
    offers.sort_by(|a, b| a.locale.cmp(&b.locale));
    offers.dedup_by(|a, b| a.locale == b.locale);
    Ok(offers)
}

pub struct LanguageArea {
    remote: RemoteActions,
    /// The offers the registries make, once the workspace answered.
    offers: LocalState<Option<Offered>>,
    /// The read of the offers under way.
    reading: RefCell<Option<InvocationCall>>,
    /// The language whose pack is being named in the tree, and the call that names it.
    installing: RefCell<Option<(String, InvocationCall)>>,
    pub installing_language: LocalState<Option<String>>,
    /// Why the last install failed.
    pub failed: LocalState<Option<String>>,
    pub shown: LocalState<u64>,
    seen: Cell<(u64, u64, u64)>,
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
            remote: RemoteActions::current(),
            offers: LocalState::new(None),
            reading: RefCell::new(None),
            installing: RefCell::new(None),
            installing_language: LocalState::new(None),
            failed: LocalState::new(None),
            shown: LocalState::new(0),
            seen: Cell::new((0, 0, 0)),
        }
    }

    fn poll(&self) {
        self.take_offers();
        self.take_install();
        let now = (
            self.installing_language.revision(),
            self.failed.revision(),
            self.offers.revision(),
        );
        if self.seen.replace(now) != now {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    /// Ask the workspace what the registries offer unless it was asked already.
    pub fn read(&self) {
        if self.offers.get().is_some() || self.reading.borrow().is_some() {
            return;
        }
        let Ok(workspace) = ui::framework::reference::workspace() else {
            self.offers.set(Some(Offered {
                languages: Vec::new(),
                unavailable: Some(t("settings-region-language-no-workspace")),
            }));
            return;
        };
        *self.reading.borrow_mut() = Some(self.remote.invoke(
            workspace_governance(&workspace, "app.offers"),
            InvokeArgument::Json(serde_json::Map::new()),
            None,
        ));
    }

    fn take_offers(&self) {
        let answered = self
            .reading
            .borrow()
            .as_ref()
            .and_then(crate::panes::person_settings::settled);
        if let Some(answered) = answered {
            *self.reading.borrow_mut() = None;
            let offered = match answered.and_then(offers_from) {
                Ok(languages) => Offered {
                    languages,
                    unavailable: None,
                },
                Err(error) => Offered {
                    languages: Vec::new(),
                    unavailable: Some(error),
                },
            };
            self.offers.set(Some(offered));
        }
    }

    /// Name the pack of `locale` in the workspace's tree, through the action every app installs
    /// through. The device's daemon installs it, and the desktop follows when it lands.
    fn install(&self, locale: &str) -> Result<(), String> {
        let offered = self
            .offers
            .get()
            .ok_or_else(|| t("settings-region-language-no-installer"))?;
        if let Some(why) = &offered.unavailable {
            return Err(why.clone());
        }
        let offer = offered
            .languages
            .iter()
            .find(|offer| offer.locale == tilo_paths::language_of(locale))
            .ok_or_else(|| {
                crate::i18n::t_args(
                    "settings-region-language-not-offered",
                    &[("language", name(Some(locale)).into())],
                )
            })?;
        let workspace = ui::framework::reference::workspace()?;
        let mut arguments = serde_json::Map::new();
        arguments.insert("app".into(), offer.app.clone().into());
        arguments.insert("version".into(), offer.version.clone().into());
        arguments.insert("package".into(), offer.package.clone().into());
        arguments.insert("publisher".into(), offer.publisher.clone().into());
        if !offer.surface.is_empty() {
            arguments.insert("surface".into(), offer.surface.clone().into());
        }
        let call = self.remote.invoke(
            workspace_governance(&workspace, "install-app"),
            InvokeArgument::Json(arguments),
            Some(Confirmation::Confirmed),
        );
        self.failed.set(None);
        self.installing_language.set(Some(offer.locale.clone()));
        *self.installing.borrow_mut() = Some((offer.locale.clone(), call));
        self.shown.set(self.shown.get().wrapping_add(1));
        Ok(())
    }

    fn take_install(&self) {
        let answered = self
            .installing
            .borrow()
            .as_ref()
            .map(|(_, call)| call.with(Clone::clone));
        let Some(answered) = answered else {
            return;
        };
        let outcome = match answered {
            Arrival::Pending { .. } => return,
            Arrival::Failed(error) => Err(format!("{error:?}")),
            Arrival::Ready(ui::tilo_actions::InvocationUpdate::Succeeded { .. }) => Ok(()),
            Arrival::Ready(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                Err(error.message)
            }
            Arrival::Ready(
                ui::tilo_actions::InvocationUpdate::Accepted { .. }
                | ui::tilo_actions::InvocationUpdate::Progress { .. }
                | ui::tilo_actions::InvocationUpdate::Item { .. },
            ) => return,
        };
        self.installing.borrow_mut().take();
        self.installing_language.set(None);
        match outcome {
            Ok(()) => {
                ui::i18n::refresh_language_packs();
                self.offers.set(None);
                self.read();
            }
            Err(error) => self.failed.set(Some(error)),
        }
        self.shown.set(self.shown.get().wrapping_add(1));
    }

    /// The language the pane shows as being installed, if one is.
    pub(crate) fn installing_locale(&self) -> Option<String> {
        self.installing_language.with(Clone::clone)
    }
}

#[must_use]
pub fn choices(installed: &[&str], offered: &Offered) -> Vec<String> {
    let shown = installed.iter().copied();
    let offered = offered.languages.iter().map(|offer| offer.locale.as_str());
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
    _cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let LanguageEffect::ChooseLanguage { language } = effect;
    let languages = area(app).ok_or(crate::area::missing)?;
    languages.read();
    let offered = languages.offers.get().unwrap_or_default();
    let chosen = resolve(
        &language,
        &choices(&crate::i18n::CATALOG.languages(), &offered),
    )
    .ok_or_else(|| format!("`{language}` is not a language this desktop has or offers"))?;
    languages.failed.set(None);
    let language = match chosen {
        Chosen::Device => None,
        Chosen::Language(language) if installed(&language) => Some(language),
        Chosen::Language(language) => {
            // Named in the workspace's tree; the pane follows the install and switches the
            // desktop when it lands.
            languages.install(&language)?;
            return Ok(Effected::done());
        }
    };
    app.sections()
        .invoke::<AppearancePrefs>(AppearancePrefs::paths().language.set(language))
        .map(|()| Effected::done())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(locale: &str) -> LanguageOffer {
        LanguageOffer {
            app: format!("so.tilo.language-pack.{locale}"),
            locale: locale.into(),
            version: "1.0.0".into(),
            package: "b".repeat(64),
            publisher: "c".repeat(64),
            surface: String::new(),
        }
    }

    #[test]
    fn german_is_one_choice_named_as_its_pack_is() {
        let offered = Offered {
            languages: vec![offer("de"), offer("fr")],
            unavailable: None,
        };
        assert_eq!(choices(&["en-US", "de-DE"], &offered), ["en", "de", "fr"]);
    }

    /// The offers the workspace answers with are its language packs, each language read from its
    /// record id; an app that is no language pack is left out.
    #[test]
    fn the_offers_are_the_language_packs_and_nothing_else() {
        let offers = offers_from(serde_json::json!({
            "offers": [
                {"app": "so.tilo.language-pack.de", "version": "1.0.0", "surface": "",
                 "package": "b".repeat(64), "publisher": "c".repeat(64), "registry": "Tilo main"},
                {"app": "so.tilo.notes", "version": "1.0.0", "surface": "e".repeat(64),
                 "package": "d".repeat(64), "publisher": "c".repeat(64), "registry": "Tilo main"},
            ]
        }))
        .unwrap();
        assert_eq!(offers.len(), 1, "{offers:?}");
        assert_eq!(offers[0].locale, "de");
        assert_eq!(offers[0].app, "so.tilo.language-pack.de");
    }
}
