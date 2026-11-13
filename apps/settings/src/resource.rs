//! Settings as a [`tilo_resource::Resource`]: the door Settings' effects and
//! live rows are published on. Settings edits no document of its own.

use ui::application::ComponentBehaviour;
use ui::framework::{Fixture, LaunchInput, ResourceApp, ViewCx};
use ui::tilo_resource::{self, Invoke, Model, resource};
use ui::widget::Element;

use crate::categories::Category;
use crate::effects::{NavigationEffect, SettingsEffect};
use crate::settings_app::SettingsApp;
use ui::tilo_resource::Effect as _;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Settings {}

impl Settings {
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "settings.root",
    search = Settings::panes,
    read_content = Settings::read_document
)]
impl Settings {}

impl Settings {
    fn panes(_: &Self) -> Vec<ui::tilo_resource::SearchEntry> {
        crate::search::pane_entries()
    }
}

/// The effect a system tool's intent opens Settings with: VPN's list, a
/// profile's page or its import. Network's pane when the VPN effects are not
/// in this build.
#[cfg(feature = "tilo-os")]
pub fn tool_effect(intent: &os_actions::ToolIntent) -> Result<SettingsEffect, String> {
    let os_actions::ToolIntent::Vpn { route, profile } = intent else {
        return Err("Settings opens only the VPN tool".into());
    };
    let mut arguments = serde_json::Map::new();
    let id = match (route, profile) {
        (os_actions::VpnRoute::Import, _) => "import-vpn-file",
        (os_actions::VpnRoute::Profile | os_actions::VpnRoute::DnsRouting, Some(profile)) => {
            arguments.insert(
                "connection".to_owned(),
                serde_json::Value::from(profile.as_str()),
            );
            "show-vpn-profile"
        }
        _ => "show-vpns",
    };
    SettingsEffect::decode(id, &arguments).or_else(|_| {
        Ok(NavigationEffect::OpenPane {
            pane: Category::Network.slug().to_owned(),
        }
        .into())
    })
}

pub fn parse_settings_uri(uri: &str) -> Result<SettingsEffect, String> {
    let rest = uri
        .strip_prefix("settings://")
        .or_else(|| uri.strip_prefix("settings:"))
        .ok_or_else(|| "not a Settings link".to_string())?;
    if let Some(id) = rest.strip_prefix("models/") {
        let id = id.trim_end_matches('/');
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.' | b'/')
            })
        {
            return Err("invalid local model id in Settings link".into());
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("model".to_owned(), serde_json::Value::from(id));
        return SettingsEffect::decode("show-local-model", &arguments)
            .map_err(|error| error.to_string());
    }
    // Pane links only select a view. A fragment naming a setting on the
    // pane (`settings://keyboard#keyboard.layout`) shows that setting; any
    // other query or fragment does not change the selection. Model links
    // retain their stricter id validation.
    let pane = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if let Some((_, fragment)) = rest.split_once('#')
        && let Some(setting) = settings_store::index::setting(fragment)
        && (setting.page == pane || pane_alias(pane) == setting.page)
    {
        return Ok(NavigationEffect::ShowSetting {
            setting: setting.id.to_owned(),
        }
        .into());
    }
    let pane = pane_alias(pane);
    let category = Category::from_slug(pane).ok_or("unknown Settings pane in link")?;
    Ok(NavigationEffect::OpenPane {
        pane: category.slug().to_owned(),
    }
    .into())
}

fn pane_alias(pane: &str) -> &str {
    match pane {
        "privacy" => "permissions",
        "models" => "agent_models",
        other => other,
    }
}

pub struct SettingsShell;

impl ResourceApp for SettingsShell {
    type Resource = Settings;
    const ID: &'static str = settings_sections::APP_ID;
    const TITLE: &'static str = "Settings";
    const TITLE_KEY: &'static str = "settings-app-title";
    const ICON: Option<&'static str> = Some("settings");
    const BEHAVIOUR: ComponentBehaviour = ComponentBehaviour {
        autofocus_on_structure_change: true,
        appearance_reconcile: true,
        ..ComponentBehaviour::DEFAULT
    };
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Settings>) -> Element {
        let app = match cx.storage().settings_document() {
            ui::storage::SettingsDocument::Session => SettingsApp::new(),
            ui::storage::SettingsDocument::Profile(root) => SettingsApp::with_config_root(root),
        };
        crate::window::window(cx, app)
    }

    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Settings>> {
        let parsed = match input.argument() {
            Some(argument) => parse_settings_uri(argument),
            #[cfg(feature = "tilo-os")]
            None => match os_actions::tool_intent_from_env() {
                Some(intent) => tool_effect(&intent),
                None => return Vec::new(),
            },
            #[cfg(not(feature = "tilo-os"))]
            None => return Vec::new(),
        };
        match parsed {
            Ok(effect) => vec![Invoke::effect(&effect)],
            Err(error) => {
                tracing::warn!(%error, "ignoring the Settings link");
                Vec::new()
            }
        }
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::names()
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        crate::scenarios::viewport(scenario)
    }

    /// Every section of the person's settings Settings edits: each is a
    /// record of the person's own, published with its field edits, so the
    /// palette, agents and assistive technology change it by path. The
    /// session keeps and publishes the system's own
    /// ([`settings_sections::declare`]); the panes' controls change them
    /// through its actions.
    fn records(records: &mut ui::framework::Records<'_>) {
        records.kept_by(
            tilo_actions::remote::settings_records::PROVIDER,
            settings_sections::declare,
        );
    }

    const PERSON_RECORDS: &'static [ui::framework::PersonKind] =
        &[ui::framework::PersonKind::shared::<
            accounts::person::PersonAccounts,
        >()];

    fn fixture(fixture: &mut Fixture<Settings>) {
        crate::scenarios::prepare(fixture);
    }
}

#[cfg(test)]
#[cfg(feature = "tilo-os")]
mod tests {
    use super::*;
    use crate::panes::agent_models::AgentModelsEffect;

    #[test]
    fn a_pane_launch_is_applied_once_and_does_not_leak_to_another_window() {
        use ui::framework::testing::TestApp;

        let mut app = TestApp::<SettingsShell>::launched(&["settings://updates"])
            .expect("the actual framework launch");
        assert_eq!(
            app.with_widget::<crate::window::Inspected, _>(|settings| settings.selected()),
            Some(Category::Updates),
            "the launch link opens its named pane"
        );
        app.effect(&SettingsEffect::from(NavigationEffect::OpenPane {
            pane: "power".into(),
        }))
        .expect("the normal public navigation effect");
        for _ in 0..3 {
            app.harness().relayout();
            assert_eq!(
                app.with_widget::<crate::window::Inspected, _>(|settings| settings.selected()),
                Some(Category::Power),
                "later host frames must not replay the consumed Updates launch"
            );
        }
        let mut reopened = app
            .reopen()
            .expect("reopen the same profile without arguments");
        assert_eq!(
            reopened.with_widget::<crate::window::Inspected, _>(|settings| settings.selected()),
            Some(Category::Power),
            "the reopened window shows the page it showed, not the consumed launch's"
        );
        let mut other = TestApp::<SettingsShell>::new().expect("another ordinary window");
        assert_eq!(
            other.with_widget::<crate::window::Inspected, _>(|settings| settings.selected()),
            Some(Category::DEFAULT),
            "a launch belongs to its window, not a global pane mailbox"
        );
        let mut invalid = TestApp::<SettingsShell>::launched(&["--shots"])
            .expect("an argument that is not a Settings link");
        assert_eq!(
            invalid.with_widget::<crate::window::Inspected, _>(|settings| settings.selected()),
            Some(Category::DEFAULT),
            "the original non-link argument never selects a pane"
        );
    }

    #[test]
    fn pane_links_preserve_query_and_fragment_compatibility() {
        for uri in [
            "settings://power?x=1",
            "settings:power?x=1",
            "settings://power#battery",
            "settings://power?x=1#battery",
        ] {
            assert_eq!(
                parse_settings_uri(uri),
                Ok(NavigationEffect::OpenPane {
                    pane: "power".into(),
                }
                .into()),
                "{uri}"
            );
        }
        for uri in [
            "settings://nope?pane=power",
            "settings://Power#battery",
            "settings://?pane=power",
            "settings://user@power?x=1",
            "settings://power:443#battery",
            "settings:///power?x=1",
            "settings://models/u2net-subject?x=1",
            "settings://models/u2net-subject#card",
        ] {
            assert!(parse_settings_uri(uri).is_err(), "{uri}");
        }
    }

    /// A fragment naming a setting on the linked page shows the setting; one
    /// naming a setting elsewhere or nothing only opens the page.
    #[test]
    fn setting_links_show_their_setting() {
        assert_eq!(
            parse_settings_uri("settings://keyboard#keyboard.layout"),
            Ok(NavigationEffect::ShowSetting {
                setting: "keyboard.layout".into()
            }
            .into())
        );
        for setting in settings_store::index::SETTINGS {
            assert_eq!(
                parse_settings_uri(&setting.link()),
                Ok(NavigationEffect::ShowSetting {
                    setting: setting.id.into()
                }
                .into()),
                "{}",
                setting.link()
            );
            assert!(
                Category::from_slug(setting.page).is_some(),
                "{} is on a page Settings has",
                setting.id
            );
        }
        assert_eq!(
            parse_settings_uri("settings://power#keyboard.layout"),
            Ok(NavigationEffect::OpenPane {
                pane: "power".into()
            }
            .into()),
            "a setting on another page does not move the link elsewhere"
        );
    }

    #[test]
    fn the_sidebar_lists_the_index_pages_in_order() {
        let slugs: Vec<&str> = Category::ALL
            .iter()
            .map(|category| category.slug())
            .collect();
        let index: Vec<&str> = settings_store::index::PAGES
            .iter()
            .map(|page| page.slug)
            .collect();
        assert_eq!(slugs, index);
    }

    #[test]
    fn settings_links_decode_to_what_they_show() {
        assert_eq!(
            parse_settings_uri("settings://updates"),
            Ok(NavigationEffect::OpenPane {
                pane: "updates".into()
            }
            .into())
        );
        assert_eq!(
            parse_settings_uri("settings://models/u2net-subject"),
            Ok(AgentModelsEffect::ShowLocalModel {
                model: "u2net-subject".into()
            }
            .into())
        );
        assert_eq!(
            parse_settings_uri("settings://privacy/camera"),
            Ok(NavigationEffect::OpenPane {
                pane: "permissions".into()
            }
            .into())
        );
        assert_eq!(
            parse_settings_uri("settings://agent_models"),
            Ok(NavigationEffect::OpenPane {
                pane: "agent_models".into()
            }
            .into())
        );
        assert!(parse_settings_uri("settings://models/").is_err());
        assert!(parse_settings_uri("settings://models/U2NET?x=1").is_err());
        assert!(parse_settings_uri("settings://nowhere").is_err());
        assert!(parse_settings_uri("https://models/u2net-subject").is_err());
    }
}
