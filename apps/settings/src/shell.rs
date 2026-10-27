//! The app every host runs: Settings' core shell, with every part of
//! Settings brought together before it opens ([`crate::install`]).

use ui::application::ComponentBehaviour;
use ui::framework::{Fixture, LaunchInput, ResourceApp, ViewCx};
use ui::tilo_resource::Invoke;
use ui::widget::Element;

use settings_core::resource::{Settings, SettingsShell as Core};

/// Framework app: the settings widget is the surface over [`Settings`].
pub struct SettingsShell;

impl ResourceApp for SettingsShell {
    type Resource = Settings;
    const ID: &'static str = Core::ID;
    const TITLE: &'static str = Core::TITLE;
    const ICON: Option<&'static str> = Core::ICON;
    const BEHAVIOUR: ComponentBehaviour = Core::BEHAVIOUR;
    const ARGUMENTS: usize = Core::ARGUMENTS;
    const OPENS_WITHOUT_IDENTITY: bool = Core::OPENS_WITHOUT_IDENTITY;

    fn view(cx: &mut ViewCx<Settings>) -> Element {
        crate::install();
        Core::view(cx)
    }

    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<Settings>> {
        crate::install();
        Core::launch(input, created)
    }

    fn scenarios() -> &'static [&'static str] {
        Core::scenarios()
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        Core::shot_viewport(scenario)
    }

    fn records(records: &mut ui::framework::Records<'_>) {
        crate::install();
        Core::records(records);
    }

    fn fixture(fixture: &mut Fixture<Settings>) {
        crate::install();
        Core::fixture(fixture);
    }
}

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::SettingsShell::scenarios() {
            let mut app = TestApp::<super::SettingsShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
