//! Everything Settings does, as one typed effect vocabulary.
//!
//! Settings edits no document of its own: a preference lives in the
//! session's settings records (read and written through the app's settings
//! document, [`crate::store`]), a device's state lives in the device. Every
//! change a control, the palette, an agent or a script makes is therefore a
//! [`SettingsEffect`], answered by [`crate::SettingsApp`]'s
//! [`ResourceSurface::effect`](ui::framework::ResourceSurface::effect).
//!
//! The vocabulary is composed of one `#[derive(Effect)]` enum per area of
//! Settings ([`settings_effects!`]): each area declares its own effects and
//! answers them in its own module, and every effect id is unique across the
//! whole vocabulary. A control binds one of an area's effects with
//! [`effect`], which names it as part of [`SettingsEffect`].

use ui::framework::{EffectAnswer, EffectCx};
use ui::tilo_resource::{self, ActionRef, Effect, EffectDecl};

use crate::categories::Category;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;

/// The operation every Settings effect travels under.
pub const OPERATION: &str = "settings.effect";

/// Declare [`SettingsEffect`] as the union of the areas' vocabularies, each
/// answered by its module's `answer(app, effect, cx)`.
macro_rules! settings_effects {
    ($($variant:ident($ty:ty) => $answer:path),* $(,)?) => {
        /// Every effect Settings answers. See the [module docs](self).
        #[derive(Clone, Debug, PartialEq)]
        pub enum SettingsEffect {
            $($variant($ty),)*
        }

        $(impl From<$ty> for SettingsEffect {
            fn from(effect: $ty) -> Self {
                Self::$variant(effect)
            }
        })*

        impl Effect for SettingsEffect {
            const OPERATION: &'static str = OPERATION;

            fn declarations() -> Vec<EffectDecl> {
                let mut declarations = Vec::new();
                $(declarations.extend(<$ty as Effect>::declarations());)*
                declarations
            }

            fn id(&self) -> &'static str {
                match self {
                    $(Self::$variant(effect) => effect.id(),)*
                }
            }

            fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
                match self {
                    $(Self::$variant(effect) => effect.arguments(),)*
                }
            }

            fn decode(
                id: &str,
                arguments: &serde_json::Map<String, serde_json::Value>,
            ) -> Result<Self, String> {
                $(if <$ty as Effect>::declarations().iter().any(|decl| decl.id == id) {
                    return <$ty as Effect>::decode(id, arguments).map(Self::$variant);
                })*
                Err(format!("Settings has no effect `{id}`"))
            }
        }

        /// Answer `effect` on `app`, in the module of its area.
        pub(crate) fn answer(
            app: &mut SettingsApp,
            effect: SettingsEffect,
            cx: &mut EffectCx<Settings, SettingsApp>,
        ) -> EffectAnswer {
            match effect {
                $(SettingsEffect::$variant(effect) => $answer(app, effect, cx),)*
            }
        }
    };
}

settings_effects! {
    Navigation(NavigationEffect) => answer_navigation,
    Appearance(crate::panes::appearance::AppearanceEffect) => crate::panes::appearance::answer,
    Accessibility(crate::panes::accessibility::AccessibilityEffect) => crate::panes::accessibility::answer,
}

/// The effects that differ from their declaration on this device now.
pub(crate) fn statuses(app: &SettingsApp) -> Vec<ui::framework::EffectStatus> {
    let software = app.software();
    crate::panes::accessibility::statuses(software.on_screen_keyboard, software.speech.is_some())
}

/// `effect` of one area, named as part of [`SettingsEffect`], for binding a
/// control: `cx.bind(toggle, effect(AppearanceEffect::SET_HIGH_CONTRAST))`.
#[must_use]
pub const fn effect<E, A: serde::Serialize>(
    effect: ActionRef<E, A>,
) -> ActionRef<SettingsEffect, A> {
    ActionRef::new(effect.id, effect.params)
}

/// Moving between the panes of Settings.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum NavigationEffect {
    /// Open a pane of Settings.
    #[effect(view, label = "Open settings pane")]
    OpenPane {
        #[param("The pane: its id, such as `appearance` or `network`")]
        pane: String,
    },
}

fn answer_navigation(
    app: &mut SettingsApp,
    effect: NavigationEffect,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    match effect {
        NavigationEffect::OpenPane { pane } => {
            let category = Category::from_slug(&pane)
                .ok_or_else(|| format!("Settings has no pane `{pane}`"))?;
            app.select(category);
            Ok(ui::framework::Effected::done())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One vocabulary: every effect id names exactly one effect, and each
    /// decodes back to the value that encoded it.
    #[test]
    fn effect_ids_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for decl in SettingsEffect::declarations() {
            assert!(
                seen.insert(decl.id),
                "effect `{}` is declared twice",
                decl.id
            );
        }
        let open = SettingsEffect::from(NavigationEffect::OpenPane {
            pane: "appearance".into(),
        });
        assert_eq!(
            SettingsEffect::decode(open.id(), &open.arguments()),
            Ok(open)
        );
        assert!(SettingsEffect::decode("no-such-effect", &serde_json::Map::new()).is_err());
    }
}
