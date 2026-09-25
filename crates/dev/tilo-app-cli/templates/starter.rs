use ui::prelude::*;

/// The regions a profile can be set to.
pub const REGIONS: [&str; 3] = ["Europe/Berlin", "America/New_York", "Asia/Tokyo"];

/// The app's state: one profile. It is stored, synced to collaborators and
/// versioned by the framework; every change to it is an action below.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    pub updates: bool,
    pub refresh_minutes: f64,
    pub volume: f64,
    /// A code, not prose: written whole, last writer wins.
    pub region: Atom,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: String::from("world"),
            updates: true,
            refresh_minutes: 15.0,
            volume: 0.65,
            region: Atom(String::from("Europe/Berlin")),
        }
    }
}

/// Every method marked `#[action]` is a typed, replayable, undoable edit that
/// the controls below, the action palette, agents and scripts all invoke.
#[resource(kind = "generated.profile", label = "Profile")]
impl Profile {
    /// Change the display name.
    #[action]
    pub fn set_name(&mut self, #[param("Display name")] name: String) {
        self.name = name;
    }

    /// Receive updates or not.
    #[action]
    pub fn set_updates(&mut self, #[param("Receive updates")] updates: bool) {
        self.updates = updates;
    }

    /// Change the refresh interval.
    #[action]
    pub fn set_refresh(
        &mut self,
        #[param("Refresh minutes, 1 to 120")] minutes: f64,
    ) -> Result<(), &'static str> {
        if !(1.0..=120.0).contains(&minutes) {
            return Err("the refresh interval is 1 to 120 minutes");
        }
        self.refresh_minutes = minutes;
        Ok(())
    }

    /// Change the volume.
    #[action]
    pub fn set_volume(
        &mut self,
        #[param("Volume from 0 to 1")] volume: f64,
    ) -> Result<(), &'static str> {
        if !(0.0..=1.0).contains(&volume) {
            return Err("the volume is between 0 and 1");
        }
        self.volume = volume;
        Ok(())
    }

    /// Change the region.
    #[action]
    pub fn set_region(&mut self, #[param("Region")] region: String) -> Result<(), &'static str> {
        if !REGIONS.contains(&region.as_str()) {
            return Err("unknown region");
        }
        self.region = Atom(region);
        Ok(())
    }

    /// Reset the profile.
    #[action]
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The app: the profile resource and a view of it.
pub struct App;

impl ResourceApp for App {
    type Resource = Profile;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Profile>) -> Element {
        crate::install_catalog();
        let name = cx
            .input(|profile| profile.name.clone(), Profile::SET_NAME)
            .with_id("name-input")
            .with_placeholder("starter-name-placeholder");
        let updates = cx
            .checkbox(
                "starter-updates-label",
                |profile| profile.updates,
                Profile::SET_UPDATES,
            )
            .with_id("updates-checkbox");
        // Any toolkit control binds the same way: its value follows the
        // resource and each committed value invokes the action.
        let refresh = cx
            .control(
                |value| {
                    NumberInput::from_state(value)
                        .with_range(1.0, 120.0)
                        .with_step(5.0)
                        .with_suffix("min")
                },
                |profile| profile.refresh_minutes,
                Profile::SET_REFRESH,
            )
            .with_id("refresh-input");
        let volume = cx
            .control(
                Slider::from_state,
                |profile| profile.volume,
                Profile::SET_VOLUME,
            )
            .with_id("volume-slider");
        let region = cx
            .control(
                |value| {
                    Dropdown::from_state(
                        REGIONS.iter().map(|region| (*region).to_owned()).collect(),
                        value,
                    )
                },
                |profile| profile.region.0.clone(),
                Profile::SET_REGION,
            )
            .with_id("region-select");
        let greeting = cx
            .text(|profile| {
                ui::i18n::I18nManager::format(
                    "starter-greeting",
                    &[("name", profile.name.as_str().into())],
                )
            })
            .with_id("greeting");
        let reset = cx.button("starter-reset", Profile::RESET.invoke());

        Box::new(ui::column![
            Text::new("starter-title").with_role(TextRole::Title),
            labeled_field("starter-name-label", name),
            updates,
            labeled_field("starter-refresh-label", refresh),
            labeled_field("starter-volume-label", volume),
            labeled_field("starter-region-label", region),
            greeting,
            reset,
        ])
    }
}
