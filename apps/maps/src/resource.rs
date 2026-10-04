//! Maps as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for the workspace document, and the effects a window answers.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface, RunMode,
    SurfaceCx, ViewCx,
};
use ui::tilo_actions::{ActionParam, ParamType};
use ui::tilo_resource::{self, ActionDecl, ActionRef, Doc, Effect, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    MAPS_ACTION_TYPE, MapsAction, document_bound_actions, document_child, document_children,
    register,
};
use crate::app::MapsApp;
use crate::doc::MapsDoc;
use crate::model::{MapModel, TravelMode};
use crate::scenario::Sample;

/// The maps workspace resource.
pub struct Maps;

impl Maps {
    /// Save a place (`true`) or remove it from the saved places.
    pub const SAVE_PLACE: ActionRef<Maps, (String, bool)> =
        ActionRef::new("save-place", &["place", "saved"]);
    /// Choose how directions travel: `car`, `transit`, `bike`, `foot`.
    pub const SET_TRAVEL_MODE: ActionRef<Maps, String> =
        ActionRef::new("set-travel-mode", &["mode"]);
    /// Drop the active road route.
    pub const CLEAR_ROUTE: ActionRef<Maps, ()> = ActionRef::new("clear-route", &[]);
    /// Drop the accepted transit options.
    pub const CLEAR_TRANSIT: ActionRef<Maps, ()> = ActionRef::new("clear-transit", &[]);
}

impl Resource for Maps {
    const KIND: &'static str = "maps.workspace";
    const LABEL: &'static str = "Maps";
    const ACTION_TYPE: &'static str = MAPS_ACTION_TYPE;

    type State = MapModel;
    type Action = MapsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<MapModel, ModelError> {
        Ok(MapsDoc::load(doc).unwrap_or_default())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                Self::SAVE_PLACE.id,
                "Save place",
                vec![
                    ActionParam::required("place", ParamType::Text, "The place's id"),
                    ActionParam::optional(
                        "saved",
                        ParamType::Boolean,
                        "false to remove it from the saved places",
                    ),
                ],
                |args, _| {
                    let id = args
                        .text("place")
                        .ok_or_else(|| "missing place".to_owned())?
                        .to_owned();
                    let saved = args.boolean("saved").unwrap_or(true);
                    Ok(MapsAction::SetPlaceSaved { id, saved })
                },
            )
            .describe("Save a known place to the shared saved places, or remove it."),
            ActionDecl::new(
                Self::SET_TRAVEL_MODE.id,
                "Set travel mode",
                vec![
                    ActionParam::required("mode", ParamType::Text, "car | transit | bike | foot")
                        .with_choices(["car", "transit", "bike", "foot"]),
                ],
                |args, _| {
                    let name = args.text("mode").ok_or_else(|| "missing mode".to_owned())?;
                    let mode: TravelMode = ui::serde_json::from_value(name.into())
                        .map_err(|_| format!("unknown travel mode {name:?}"))?;
                    Ok(MapsAction::SetTravelMode { mode })
                },
            )
            .describe("Choose how directions travel."),
            ActionDecl::new(Self::CLEAR_ROUTE.id, "Clear route", Vec::new(), |_, _| {
                Ok(MapsAction::ClearRoute)
            })
            .describe("Drop the active road route."),
            ActionDecl::new(
                Self::CLEAR_TRANSIT.id,
                "Clear transit options",
                Vec::new(),
                |_, _| Ok(MapsAction::ClearTransit),
            )
            .describe("Drop the accepted transit options."),
        ]
    }

    fn bound_actions(state: &MapModel) -> Vec<tilo_actions::ActionSpec> {
        document_bound_actions(state)
    }

    fn children(state: &MapModel) -> Vec<tilo_actions::Resource> {
        document_children(state)
    }

    fn child(state: &MapModel, id: &str) -> Option<tilo_actions::Resource> {
        document_child(state, id)
    }
}

/// What a maps window does with no change to the shared map: look things
/// up, find where the device is, show what it holds, and manage the tiles
/// this device keeps for offline use.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "maps.effect")]
pub enum MapsEffect {
    /// Search maps.
    #[effect(permission = "read")]
    Search {
        #[param("Place, address, or category, e.g. museum near me")]
        query: String,
    },
    /// Use my location.
    #[effect(job, permission = "read")]
    Locate,
    /// Show traffic.
    #[effect(permission = "read")]
    ToggleTraffic,
    /// Open place.
    #[effect(permission = "read")]
    OpenPlace {
        #[param("The place's id")]
        place: String,
    },
    /// Get directions.
    #[effect(permission = "read")]
    Directions {
        #[param("The place's id")]
        place: String,
    },
    /// Open point of interest.
    #[effect(permission = "read")]
    OpenPoi {
        #[param("The point of interest's id")]
        poi: String,
    },
    /// Get directions to point of interest.
    #[effect(permission = "read")]
    PoiDirections {
        #[param("The point of interest's id")]
        poi: String,
    },
    /// Delete offline area.
    #[effect(destructive, permission = "read")]
    DeleteOfflineArea {
        #[param("The offline area's id")]
        area: String,
    },
    /// Clear offline areas.
    #[effect(destructive, permission = "read")]
    ClearOfflineAreas,
}

/// Framework app: the maps widget is the surface over [`Maps`].
pub struct MapsShell;

impl ResourceApp for MapsShell {
    type Resource = Maps;
    const ID: &'static str = "so.tilo.maps";
    const TITLE: &'static str = "Maps";
    const ICON: Option<&'static str> = Some("maps");

    fn scenarios() -> &'static [&'static str] {
        crate::scenario::SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        crate::scenario::viewport(scenario)
    }

    /// A capture shows the bundled sample city instead of live imagery.
    fn fixture(fixture: &mut Fixture<Maps>) {
        crate::scenario::prepare(fixture);
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Maps>> {
        crate::scenario::actions(name)
    }

    fn view(cx: &mut ViewCx<Maps>) -> Element {
        // Only a live window fetches tiles, places and routes; a capture
        // draws its sample, and a past version draws what it held.
        let app = match cx.mode() {
            // The tiles are kept in the app's own cache area on this device.
            RunMode::Live => match cx.storage().files().paths().cache().native_path() {
                Ok(cache) => MapsApp::new_with_cache_dir(cache.join("tiles")),
                Err(_) => MapsApp::new(),
            },
            RunMode::Capture => MapsApp::sample(
                cx.fixture::<Sample>()
                    .map_or(Sample::City, |sample| *sample),
            ),
            RunMode::Past => MapsApp::past(),
        };
        cx.surface(app)
    }
}

impl ResourceSurface<Maps, MapsEffect> for MapsApp {
    fn present(&mut self, state: &Rc<MapModel>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Maps>) {
        // A live window keeps this device's offline areas; a capture and a
        // past version show what they were staged with.
        let offline_areas = cx.mode().is_live().then(|| cx.settings());
        self.attach_framework(cx.dispatcher(), offline_areas);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(scenario) = scenario {
            self.stage_scenario(scenario);
        }
    }

    fn effect(&mut self, effect: MapsEffect, cx: &mut EffectCx<Maps, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// Ranked search hits, nearby points of interest, the saved places and
    /// recent searches to open again, and this device's offline areas: what
    /// the window shows, with what it can do there. Not document state.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.published_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.published_row(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Category, LatLon, Place};
    use crate::offline_areas::OfflineAreas;
    use ui::framework::testing::TestApp;

    /// The ids of the actions published on `resource` in the running app.
    fn actions_on(app: &mut TestApp<MapsShell>, resource: &str) -> Vec<String> {
        app.agent_surface()
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["resource"] == resource)
            .map(|row| row["action"].as_str().unwrap().to_owned())
            .collect()
    }

    fn label_of(app: &mut TestApp<MapsShell>, resource: &str, action: &str) -> String {
        app.agent_surface()
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["resource"] == resource && row["action"] == action)
            .map(|row| row["label"].as_str().unwrap().to_owned())
            .unwrap_or_default()
    }

    #[test]
    fn a_window_publishes_its_effects_beside_the_map_edits() {
        let mut app = TestApp::<MapsShell>::new().unwrap();
        let workspace = actions_on(&mut app, "maps.workspace");
        for id in [
            "search",
            "locate",
            "toggle-traffic",
            "clear-offline-areas",
            "save-place",
            "set-travel-mode",
            "clear-route",
        ] {
            assert!(
                workspace.iter().any(|action| action == id),
                "{id}: {workspace:?}"
            );
        }
        assert_eq!(
            label_of(&mut app, "maps.workspace", "toggle-traffic"),
            "Show traffic"
        );
        app.effect(&MapsEffect::ToggleTraffic).unwrap();
        assert_eq!(
            label_of(&mut app, "maps.workspace", "toggle-traffic"),
            "Hide traffic",
            "the published label follows the overlay"
        );
        assert!(
            app.effect(&MapsEffect::OpenPlace {
                place: "nowhere".into()
            })
            .is_err(),
            "an unknown place is refused"
        );
    }

    #[test]
    fn a_saved_place_is_a_shared_row_the_window_opens() {
        let mut app = TestApp::<MapsShell>::new().unwrap();
        let place = Place::new(
            "marienplatz",
            "Marienplatz",
            Category::Landmark,
            "",
            LatLon::new(48.137, 11.575),
        );
        app.invoke(
            Invoke::apply(&MapsAction::UpsertPlaces {
                places: vec![place],
            })
            .unwrap(),
        )
        .unwrap();
        app.invoke(Maps::SAVE_PLACE.with(("marienplatz".into(), true)))
            .unwrap();
        assert!(app.state().places.iter().any(|place| place.saved));
        let row = actions_on(&mut app, "maps.place:marienplatz");
        for id in ["unsave", "open-place", "directions"] {
            assert!(row.iter().any(|action| action == id), "{id}: {row:?}");
        }
        app.effect(&MapsEffect::OpenPlace {
            place: "marienplatz".into(),
        })
        .unwrap();
    }

    #[test]
    fn offline_areas_stay_on_this_device() {
        let mut app = TestApp::<MapsShell>::new().unwrap();
        let area = crate::sample::regions().remove(0);
        app.settings::<OfflineAreas>()
            .set(&OfflineAreas {
                offline_areas: vec![area.clone()],
            })
            .unwrap();
        app.harness().relayout();
        let row = format!("maps.region:{}", area.id);
        assert_eq!(actions_on(&mut app, &row), ["delete-offline-area"]);
        app.effect_confirmed(&MapsEffect::DeleteOfflineArea { area: area.id })
            .unwrap();
        assert!(
            app.settings::<OfflineAreas>()
                .get()
                .offline_areas
                .is_empty(),
            "deleting an area forgets it on this device"
        );
        assert!(actions_on(&mut app, &row).is_empty());
        assert!(
            app.state().regions.is_empty(),
            "the shared map never held the area"
        );
    }

    #[test]
    fn every_scenario_stages_its_places_and_plans_through_map_actions() {
        use ui::framework::testing::TestApp;
        // Coordinates are stored rounded, so a place is compared by what
        // names it.
        let by_id = |places: &[Place]| {
            let mut places: Vec<_> = places
                .iter()
                .map(|place| (place.id.clone(), place.name.clone(), place.saved))
                .collect();
            places.sort();
            places
        };
        let sample = crate::sample::default_model();
        for scenario in MapsShell::scenarios() {
            let app = TestApp::<MapsShell>::capture(Some(scenario)).unwrap();
            let state = app.state();
            if matches!(*scenario, "empty" | "traffic-empty") {
                assert!(state.places.is_empty(), "`{scenario}` is a first run");
                assert!(state.route.is_none(), "`{scenario}` plans nothing");
                continue;
            }
            assert_eq!(
                by_id(&state.places),
                by_id(&sample.places),
                "`{scenario}` holds the city"
            );
            assert_eq!(state.recents, sample.recents, "`{scenario}` searched");
            assert_eq!(
                state.transit.len(),
                sample.transit.len(),
                "`{scenario}` planned transit"
            );
            match *scenario {
                "transit" | "transit-arrive" => {
                    assert_eq!(state.mode, TravelMode::Transit);
                    assert!(state.route.is_none(), "`{scenario}` draws no road");
                }
                "pois" | "offline" => assert!(state.route.is_none()),
                _ => {
                    assert_eq!(
                        state.route.as_ref().map(|route| route.geometry.len()),
                        sample.route.as_ref().map(|route| route.geometry.len()),
                        "`{scenario}` routes"
                    );
                    assert_eq!(state.mode, sample.mode);
                    assert_eq!(state.dest_name, sample.dest_name);
                }
            }
            assert!(state.regions.is_empty(), "offline areas are not the map's");
        }
        let arrive = TestApp::<MapsShell>::capture(Some("transit-arrive"))
            .unwrap()
            .state()
            .transit_time;
        assert_eq!(arrive.offset_min, 60);
        assert!(arrive.arrive_by());
    }
}
