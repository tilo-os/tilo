//! Settings as a [`tilo_resource::Resource`]: the door Settings' effects and
//! live rows are published on.
//!
//! Settings edits no document of its own. The person's and the device's
//! preferences are the session's settings records, which every effect reads
//! and writes through the app's settings document; this device's hardware
//! and services are its own. So the resource holds nothing: it names
//! Settings to the palette, agents and scripts, and every change is a
//! [`SettingsEffect`].

use std::rc::Rc;

use ui::application::ComponentBehaviour;
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_resource::{self, Invoke, Model, resource};
use ui::widget::Element;

use crate::categories::Category;
use crate::effects::{NavigationEffect, SettingsEffect};
use crate::host::PaneCx;
use crate::settings_app::SettingsApp;
use ui::tilo_resource::Effect as _;

/// The Settings resource: the door its effects and live rows are published
/// on. It holds nothing (see the module docs).
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Settings {}

#[resource(kind = "settings.root", label = "Settings", search = Settings::panes)]
impl Settings {}

impl Settings {
    /// Every pane, found by its name, summary and the words people use for
    /// it.
    fn panes(_: &Self) -> Vec<ui::tilo_resource::SearchEntry> {
        crate::search::pane_entries()
    }
}

/// Decode a `settings://` link: `settings://models/<model-id>` shows that
/// local model's card and `settings://<pane>[/…]` opens a pane. The launcher
/// already checked the scheme; this keeps a malformed value from becoming a
/// model or pane id.
///
/// # Errors
/// Returns why `uri` is not a Settings link this version understands.
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
        return SettingsEffect::decode("show-local-model", &arguments);
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
    // Camera and other apps link to the privacy controls by their common
    // name; the pane is Permissions.
    let pane = pane_alias(pane);
    let category = Category::from_slug(pane).ok_or("unknown Settings pane in link")?;
    Ok(NavigationEffect::OpenPane {
        pane: category.slug().to_owned(),
    }
    .into())
}

/// The pane a link names by its common name: Camera and other apps link to
/// the privacy controls as `privacy`.
fn pane_alias(pane: &str) -> &str {
    match pane {
        "privacy" => "permissions",
        "models" => "agent_models",
        other => other,
    }
}

/// Framework app: the settings widget is the surface over [`Settings`].
pub struct SettingsShell;

impl ResourceApp for SettingsShell {
    type Resource = Settings;
    const ID: &'static str = "so.tilo.settings";
    const TITLE: &'static str = "Settings";
    const ICON: Option<&'static str> = Some("settings");
    // Sidebar filtering rebuilds the widget tree, and the Appearance pane's
    // scale, theme, contrast and motion apply live.
    const BEHAVIOUR: ComponentBehaviour = ComponentBehaviour {
        autofocus_on_structure_change: true,
        appearance_reconcile: true,
        ..ComponentBehaviour::DEFAULT
    };
    /// A `settings://` link.
    const ARGUMENTS: usize = 1;
    /// The device's settings belong to no workspace: they open while the
    /// workspace has no identity selected, and only the pages about that
    /// identity ask for one (`Category::needs_workspace_identity`).
    const OPENS_WITHOUT_IDENTITY: bool = true;

    fn view(cx: &mut ViewCx<Settings>) -> Element {
        // Private host profiles also own their account stores and mutation journal.
        let app = match cx.storage().settings_document() {
            ui::storage::SettingsDocument::Session => SettingsApp::new(),
            ui::storage::SettingsDocument::File(path) => {
                SettingsApp::with_config_root(path.parent().unwrap_or(path.as_path()))
            }
        };
        // A pane search hit, or an agent's `open`, shows the pane it names.
        cx.on_open(app.opener());
        cx.surface(app)
    }

    /// `settings://models/<id>` opens Agent Models at that model's card and
    /// `settings://<pane>` opens the pane. Neither changes anything.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Settings>> {
        let Some(argument) = input.argument() else {
            return Vec::new();
        };
        match parse_settings_uri(argument) {
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
    /// palette, agents and assistive technology change it by path.
    fn records(records: &mut ui::framework::Records<'_>) {
        records
            .settings::<crate::prefs::AppearancePrefs>()
            .settings::<crate::accessibility_settings::AccessibilitySettings>()
            .settings::<crate::prefs::InputPrefs>()
            .settings::<ui::text_assistance::TextAssistancePrefs>()
            .settings::<crate::prefs::PowerPrefs>()
            .settings::<crate::prefs::DeskWidgetsPrefs>()
            .settings::<crate::prefs::UpdatePrefs>()
            .settings::<crate::prefs::NotificationPrefs>()
            .settings::<crate::prefs::AudioPrefs>()
            .settings::<crate::prefs::DisplayPrefs>()
            .settings::<tilo_locale::Locale>()
            .settings::<crate::continuity::ContinuityPrivacy>();
        for section in &crate::area::registry().sections {
            (section.record)(records);
        }
    }

    fn fixture(fixture: &mut Fixture<Settings>) {
        crate::scenarios::prepare(fixture);
    }
}

impl ResourceSurface<Settings, SettingsEffect> for SettingsApp {
    fn present(&mut self, _state: &Rc<Settings>) {}

    fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        self.areas()
            .iter()
            .find_map(crate::area::PaneArea::take_host_intent)
    }

    fn attach(&mut self, cx: SurfaceCx<Settings>) {
        let accounts = cx.person::<accounts::person::PersonAccounts>();
        self.attach_host(PaneCx::attached(&cx), accounts);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scene(crate::scenarios::staged(scenario));
    }

    fn effect(
        &mut self,
        effect: SettingsEffect,
        cx: &mut EffectCx<Settings, Self>,
    ) -> EffectAnswer {
        crate::effects::answer(self, effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        crate::effects::statuses(self)
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        crate::effects::rows(self)
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        crate::effects::row(self, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panes::agent_models::AgentModelsEffect;

    #[test]
    fn a_pane_launch_is_applied_once_and_does_not_leak_to_another_window() {
        use ui::framework::testing::TestApp;

        let mut app = TestApp::<SettingsShell>::launched(&["settings://updates"])
            .expect("the actual framework launch");
        assert_eq!(
            app.with_widget::<SettingsApp, _>(|settings| settings.selected()),
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
                app.with_widget::<SettingsApp, _>(|settings| settings.selected()),
                Some(Category::Power),
                "later host frames must not replay the consumed Updates launch"
            );
        }
        let mut reopened = app
            .reopen()
            .expect("reopen the same profile without arguments");
        assert_eq!(
            reopened.with_widget::<SettingsApp, _>(|settings| settings.selected()),
            Some(Category::Power),
            "the reopened window shows the page it showed, not the consumed launch's"
        );
        let mut other = TestApp::<SettingsShell>::new().expect("another ordinary window");
        assert_eq!(
            other.with_widget::<SettingsApp, _>(|settings| settings.selected()),
            Some(Category::DEFAULT),
            "a launch belongs to its window, not a global pane mailbox"
        );
        let mut invalid = TestApp::<SettingsShell>::launched(&["--shots"])
            .expect("an argument that is not a Settings link");
        assert_eq!(
            invalid.with_widget::<SettingsApp, _>(|settings| settings.selected()),
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

    /// Settings shows every page of the index, in its order: the sidebar and
    /// the desktop palette list the same pages.
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

    /// Model links name a validated model id, pane links a known pane, and
    /// anything else is refused rather than guessed.
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
