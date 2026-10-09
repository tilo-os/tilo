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
                let build = || {
                    let mut declarations = Vec::new();
                    $(declarations.extend(<$ty as Effect>::declarations());)*
                    declarations
                };
                #[cfg(test)]
                return ui::framework::profiling::measure("settings.flattened_declarations", build);
                #[cfg(not(test))]
                build()
            }

            fn id(&self) -> &'static str {
                match self {
                    $(Self::$variant(effect) => effect.id(),)*
                }
            }

            // A row binds one area's effect. Flattening every area's
            // declarations here repeats unrelated metadata work for each row.
            // The child vocabulary shares this union's operation and payload.
            fn bound(&self) -> tilo_actions::ActionSpec {
                match self {
                    $(Self::$variant(effect) => effect.bound(),)*
                }
            }

            fn bound_asking(&self, asking: &[&str]) -> tilo_actions::ActionSpec {
                match self {
                    $(Self::$variant(effect) => effect.bound_asking(asking),)*
                }
            }

            fn bound_asking_params(&self, params: Vec<tilo_actions::ActionParam>) -> tilo_actions::ActionSpec {
                match self {
                    $(Self::$variant(effect) => effect.bound_asking_params(params),)*
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
    Notifications(crate::panes::notifications::NotificationEffect) => crate::panes::notifications::answer,
    DefaultApps(crate::panes::personal_apps::DefaultAppsEffect) => crate::panes::personal_apps::answer,
    Region(crate::panes::region::RegionEffect) => crate::panes::region::answer,
    TimeDate(crate::panes::time_date::TimeDateEffect) => crate::panes::time_date::answer,
    Shortcuts(crate::panes::keyboard_shortcuts::ShortcutEffect) => crate::panes::keyboard_shortcuts::answer,
    Input(crate::panes::mouse_keyboard::InputEffect) => crate::panes::mouse_keyboard::answer,
    Power(crate::panes::power::PowerEffect) => crate::panes::power::answer,
    Updates(crate::panes::updates::UpdatesEffect) => crate::panes::updates::answer,
    DiskUnlock(crate::panes::disk_unlock::DiskUnlockEffect) => crate::panes::disk_unlock::answer,
    Storage(crate::panes::storage::StorageEffect) => crate::panes::storage::answer,
    About(crate::panes::about::AboutEffect) => crate::panes::about::answer,
    Permissions(crate::panes::permissions::PermissionsEffect) => crate::panes::permissions::answer,
    Printers(crate::panes::printers::PrinterEffect) => crate::panes::printers::answer,
    Users(crate::panes::users::UsersEffect) => crate::panes::users::answer,
    Bluetooth(crate::panes::bluetooth::BluetoothEffect) => crate::panes::bluetooth::answer,
    Speakers(crate::panes::speakers::SpeakerEffect) => crate::panes::speakers::answer,
    SpeakerFlow(crate::panes::speakers::SpeakerFlow) => crate::panes::speakers::answer_flow,
    AgentModels(crate::panes::agent_models::AgentModelsEffect) => crate::panes::agent_models::answer,
    DeviceMesh(crate::panes::device_mesh::DeviceMeshEffect) => crate::panes::device_mesh::answer,
    Audio(crate::panes::audio::AudioEffect) => crate::panes::audio::answer,
    Accounts(crate::panes::accounts::AccountsEffect) => crate::panes::accounts::answer,
    Displays(crate::panes::display::DisplayEffect) => crate::panes::display::answer,
    Network(crate::panes::network::NetworkEffect) => crate::panes::network::answer,
}

/// The live rows of every area — the devices, networks, accounts and other
/// entities outside any document that Settings shows — with the effects each
/// offers bound to it.
pub(crate) fn rows(app: &SettingsApp) -> Vec<ui::framework::LiveRow> {
    macro_rules! observed {
        ($name:literal, $operation:expr) => {{
            #[cfg(test)]
            let result = ui::framework::profiling::measure($name, || $operation);
            #[cfg(not(test))]
            let result = $operation;
            result
        }};
    }
    let mut rows = observed!("settings.rows.panes", pane_rows());
    rows.extend(observed!(
        "settings.rows.notifications",
        app.notifications().rows()
    ));
    rows.extend(observed!(
        "settings.rows.default_apps",
        app.default_apps().rows()
    ));
    rows.extend(observed!("settings.rows.input", app.input().rows()));
    rows.extend(observed!("settings.rows.printers", app.printers().rows()));
    rows.extend(observed!("settings.rows.users", app.users().rows()));
    rows.extend(observed!("settings.rows.bluetooth", app.bluetooth().rows()));
    rows.extend(observed!("settings.rows.speakers", app.speakers().rows()));
    rows.extend(observed!(
        "settings.rows.agent_models",
        app.agent_models().rows()
    ));
    rows.extend(observed!(
        "settings.rows.device_mesh",
        app.device_mesh().rows()
    ));
    rows.extend(observed!("settings.rows.audio", app.audio().rows()));
    rows.extend(observed!("settings.rows.accounts", app.accounts().rows()));
    rows.extend(observed!("settings.rows.displays", app.displays().rows()));
    rows.extend(observed!("settings.rows.network", app.network().rows()));
    rows.extend(observed!(
        "settings.rows.disk_unlock",
        app.disk_unlock().rows()
    ));
    rows
}

/// The live row `id`.
pub(crate) fn row(app: &SettingsApp, id: &str) -> Option<ui::framework::LiveRow> {
    fn find(rows: Vec<ui::framework::LiveRow>, id: &str) -> Option<ui::framework::LiveRow> {
        rows.into_iter().find(|row| row.id() == id)
    }
    find(rows(app), id)
}

/// The effects that differ from their declaration on this device now.
pub(crate) fn statuses(app: &SettingsApp) -> Vec<ui::framework::EffectStatus> {
    let software = app.software();
    crate::panes::accessibility::statuses(software.speech.is_some())
}

/// `effect` of one area, named as part of [`SettingsEffect`], for binding a
/// control: `cx.bind(toggle, effect(AppearanceEffect::SET_HIGH_CONTRAST))`.
#[must_use]
pub const fn effect<E, A: serde::Serialize>(
    effect: ActionRef<E, A>,
) -> ActionRef<SettingsEffect, A> {
    ActionRef::new(effect.id, effect.params)
}

/// The live row of `category`'s pane.
#[must_use]
pub fn pane_row_id(category: Category) -> String {
    format!("settings.section:{}", category.slug())
}

/// What opens `category`'s pane, for search, agents and accessibility: its
/// row's [`NavigationEffect::OpenPane`].
#[must_use]
pub fn open_pane_address(category: Category) -> tilo_actions::ActionAddress {
    let open = NavigationEffect::OpenPane {
        pane: category.slug().to_owned(),
    };
    tilo_actions::ActionAddress::new(pane_row_id(category), open.id())
}

/// One row per pane, each opening it.
fn pane_rows() -> Vec<ui::framework::LiveRow> {
    Category::ALL
        .iter()
        .map(|category| {
            ui::framework::LiveRow::new(
                pane_row_id(*category),
                "settings.section",
                category.label(),
            )
            .operation(
                SettingsEffect::from(NavigationEffect::OpenPane {
                    pane: category.slug().to_owned(),
                })
                .bound(),
            )
        })
        .collect()
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
    app: &SettingsApp,
    effect: NavigationEffect,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    match effect {
        NavigationEffect::OpenPane { pane } => {
            let category = Category::from_slug(&pane)
                .ok_or_else(|| format!("Settings has no pane `{pane}`"))?;
            app.request_pane(category);
            Ok(ui::framework::Effected::done())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Flattened(SettingsEffect);

    #[test]
    fn mounted_live_rows_do_not_rebuild_unrelated_pane_declarations() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let (rows, phases) = app
            .surface::<SettingsApp, _>(|app| ui::framework::profiling::observe(|| super::rows(app)))
            .unwrap();
        assert!(
            rows.iter()
                .any(|row| row.id() == "settings.display-output:eDP-1")
        );
        assert!(
            !phases
                .iter()
                .any(|phase| phase.phase == "settings.flattened_declarations"),
            "actual mounted rows must bind only their own pane vocabulary"
        );
    }

    #[test]
    fn mounted_display_asking_bindings_preserve_metadata_payload_and_output_custody() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let rows = app
            .surface::<SettingsApp, _>(|app| super::rows(app))
            .unwrap();
        let row = rows
            .into_iter()
            .find(|row| row.id() == "settings.display-output:eDP-1")
            .expect("the actual connected output is published")
            .into_resource();
        let mut compared = 0;
        for spec in row.actions.iter().filter(|spec| !spec.params().is_empty()) {
            let mut args = serde_json::Map::new();
            for param in spec.params() {
                let value = param.choices.first().map_or_else(
                    || match param.ty {
                        tilo_actions::ParamType::Text => serde_json::json!("observed"),
                        tilo_actions::ParamType::Integer => serde_json::json!(20),
                        tilo_actions::ParamType::Number => serde_json::json!(0.5),
                        tilo_actions::ParamType::Boolean => serde_json::json!(true),
                        other => panic!("unexpected actual display parameter: {other:?}"),
                    },
                    |value| serde_json::json!(value),
                );
                args.insert(param.name.to_string(), value);
            }
            let published = spec.build_json(&args).unwrap();
            let effect = SettingsEffect::from_payload(&published.payload).unwrap();
            let reference = Flattened(effect.clone()).bound_asking_params(spec.params().to_vec());
            let direct = effect.bound_asking_params(spec.params().to_vec());
            let survey = |spec| {
                tilo_actions::door::survey(&[tilo_actions::Resource::new(
                    "comparison",
                    "comparison",
                    "comparison",
                )
                .with_action(spec)])
            };
            assert_eq!(survey(direct.clone()), survey(reference.clone()));
            assert_eq!(
                direct.permission_for_json(&args),
                reference.permission_for_json(&args)
            );
            assert_eq!(
                direct.confirmation_for_json(&args),
                reference.confirmation_for_json(&args)
            );
            let actual = direct.build_json(&args).unwrap();
            let expected = reference.build_json(&args).unwrap();
            assert_eq!(actual.type_id, expected.type_id);
            assert_eq!(actual.payload, expected.payload);
            assert_eq!(actual.payload, published.payload);
            let asking: Vec<_> = spec.params().iter().map(|param| &*param.name).collect();
            assert_eq!(
                survey(effect.bound_asking(&asking)),
                survey(Flattened(effect).bound_asking(&asking))
            );
            args.insert("display".to_string(), serde_json::json!("foreign-output"));
            assert!(
                spec.build_json(&args).is_err(),
                "the output identity is not caller controlled"
            );
            compared += 1;
        }
        assert!(
            compared >= 7,
            "the actual output's parameter actions are observed"
        );
    }

    impl Effect for Flattened {
        const OPERATION: &'static str = super::OPERATION;

        fn declarations() -> Vec<EffectDecl> {
            SettingsEffect::declarations()
        }

        fn id(&self) -> &'static str {
            self.0.id()
        }

        fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
            self.0.arguments()
        }

        fn decode(
            id: &str,
            arguments: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<Self, String> {
            SettingsEffect::decode(id, arguments).map(Self)
        }
    }

    #[test]
    fn mounted_live_row_bindings_preserve_flattened_metadata_and_payloads() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let resource_rows = app.surface::<SettingsApp, _>(|app| rows(app)).unwrap();
        let mut compared = 0;
        for row in resource_rows {
            for spec in row.into_resource().actions {
                if spec.operation_type.as_deref() != Some(super::OPERATION)
                    || !spec.params().is_empty()
                {
                    continue;
                }
                let published = spec.build_json(&serde_json::Map::new()).unwrap();
                let effect = SettingsEffect::from_payload(&published.payload).unwrap();
                let direct = effect.bound();
                let flattened = Flattened(effect).bound();
                assert_eq!(
                    tilo_actions::door::survey(&[tilo_actions::Resource::new(
                        "comparison",
                        "comparison",
                        "comparison"
                    )
                    .with_action(direct.clone())]),
                    tilo_actions::door::survey(&[tilo_actions::Resource::new(
                        "comparison",
                        "comparison",
                        "comparison"
                    )
                    .with_action(flattened.clone())]),
                    "all published metadata including role/risk/result and confirmation is preserved"
                );
                assert_eq!(
                    direct.permission_for_json(&serde_json::Map::new()),
                    flattened.permission_for_json(&serde_json::Map::new()),
                    "invocation permission classification is unchanged",
                );
                assert_eq!(
                    direct.confirmation_for_json(&serde_json::Map::new()),
                    flattened.confirmation_for_json(&serde_json::Map::new()),
                    "bound dynamic risk retains the same confirmation policy",
                );
                let actual = direct.build_json(&serde_json::Map::new()).unwrap();
                let reference = flattened.build_json(&serde_json::Map::new()).unwrap();
                assert_eq!(actual.type_id, reference.type_id);
                assert_eq!(actual.payload, reference.payload);
                assert_eq!(
                    actual.payload, published.payload,
                    "the actual published row carries the same effect arguments"
                );
                compared += 1;
            }
        }
        assert!(
            compared >= Category::ALL.len(),
            "the actual mounted pane/live rows are observed"
        );
    }

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
