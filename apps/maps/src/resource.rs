//! Maps as a resource: the shared map ([`Workspace`]) is the document, and
//! the effects a window answers look things up, find the device and manage
//! this device's offline tiles.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface, RunMode,
    SurfaceCx, ViewCx,
};
use ui::tilo_resource::{self, Effect, Invoke, SearchEntry};
use ui::widget::Element;

use crate::app::MapsApp;
use crate::scenario::Sample;
use crate::workspace::Workspace;

/// What a maps window does with no change to the shared map: look things
/// up, find where the device is, show what it holds, and manage the tiles
/// this device keeps for offline use.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "maps.effect")]
pub enum MapsEffect {
    /// Search maps.
    #[effect(kind = "read")]
    Search {
        #[param("Place, address, or category, e.g. museum near me")]
        query: String,
    },
    /// Use my location.
    #[effect(job, kind = "read")]
    Locate,
    /// Show traffic.
    #[effect(kind = "read")]
    ToggleTraffic,
    /// Open place.
    #[effect(kind = "read")]
    OpenPlace {
        #[param("The place's id")]
        place: String,
    },
    /// Get directions.
    #[effect(kind = "read")]
    Directions {
        #[param("The place's id")]
        place: String,
    },
    /// Open point of interest.
    #[effect(kind = "read")]
    OpenPoi {
        #[param("The point of interest's id")]
        poi: String,
    },
    /// Get directions to point of interest.
    #[effect(kind = "read")]
    PoiDirections {
        #[param("The point of interest's id")]
        poi: String,
    },
    /// Delete offline area.
    #[effect(destructive, kind = "read")]
    DeleteOfflineArea {
        #[param("The offline area's id")]
        area: String,
    },
    /// Clear offline areas.
    #[effect(destructive, kind = "read")]
    ClearOfflineAreas,
}

/// Framework app: the maps widget is the surface over the shared [`Workspace`].
pub struct MapsShell;

impl ResourceApp for MapsShell {
    type Resource = Workspace;
    const ID: &'static str = "so.tilo.maps";
    const TITLE: &'static str = "Maps";
    const ICON: Option<&'static str> = Some("maps");
    /// A launch may name a search (`geo:0,0?q=cafe`).
    const ARGUMENTS: usize = 1;

    fn scenarios() -> &'static [&'static str] {
        crate::scenario::SCENARIOS
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        crate::scenario::viewport(scenario)
    }

    /// A capture shows the bundled sample city instead of live imagery.
    fn fixture(fixture: &mut Fixture<Workspace>) {
        crate::scenario::prepare(fixture);
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Workspace>> {
        crate::scenario::actions(name)
    }

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        // Only a live window fetches tiles, places and routes; a capture
        // draws its sample, and a past version draws what it held.
        let mut app = match cx.mode() {
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
        // A live window launched to search ("Search Maps for cafe" in
        // desktop search) opens on the places it finds.
        if cx.mode() == RunMode::Live
            && let Some(query) = launch_search(cx.launch_arguments())
        {
            app.search_for(query);
        }
        // A search hit, or an agent's `open`, opens the place, point of
        // interest or recent search it names.
        let dispatcher = cx.dispatcher();
        cx.on_open(
            move |anchor: &str| -> Result<(), ui::tilo_resource::Refusal> {
                let effect = crate::place_search::PlaceHit::opened(anchor).ok_or_else(|| {
                    ui::tilo_resource::Refusal::new(format!("`{anchor}` is nothing on the map"))
                })?;
                dispatcher.effect(&effect);
                Ok(())
            },
        );
        cx.surface(app)
    }
}

impl ResourceSurface<Workspace, MapsEffect> for MapsApp {
    fn present(&mut self, state: &Rc<Workspace>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Workspace>) {
        // A live window keeps this device's offline areas; a capture and a
        // past version show what they were staged with.
        let offline_areas = cx.mode().is_live().then(|| cx.settings());
        self.attach_framework(cx.dispatcher(), offline_areas);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: MapsEffect, cx: &mut EffectCx<Workspace, Self>) -> EffectAnswer {
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

    /// The geocoder looks up what the platform search asks for, in the
    /// background.
    fn search_query(&mut self, query: &ui::search::SearchQuery) {
        self.queue_place_search(query);
    }

    /// The places, saved first, the points of interest around the view, the
    /// recent searches and the geocoder's places for the latest query, each
    /// opening its row.
    fn search_entries(&self) -> Vec<SearchEntry> {
        self.place_search_entries()
    }
}

/// The search a launch names: a `geo:` URI's `q` (`geo:0,0?q=cafe%20berlin`),
/// decoded; `None` for any other launch, and for an empty query.
fn launch_search(arguments: &[String]) -> Option<String> {
    arguments.iter().find_map(|arg| {
        let (scheme, rest) = arg.split_once(':')?;
        if !scheme.eq_ignore_ascii_case("geo") || arg.len() > 8 * 1024 {
            return None;
        }
        let (_, parameters) = rest.split_once('?')?;
        let query = parameters.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "q").then(|| percent_decode(value)).flatten()
        })?;
        let query = query.trim();
        (!query.is_empty() && !query.chars().any(char::is_control)).then(|| query.to_owned())
    })
}

/// `text` with `+` as a space and `%XX` as its byte; `None` when an escape
/// is malformed or the bytes are not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.bytes();
    while let Some(byte) = rest.next() {
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => {
                let high = char::from(rest.next()?).to_digit(16)?;
                let low = char::from(rest.next()?).to_digit(16)?;
                bytes.push(u8::try_from(high * 16 + low).ok()?);
            }
            _ => bytes.push(byte),
        }
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_geo_launch_names_its_search() {
        assert_eq!(
            launch_search(&["geo:0,0?q=caf%C3%A9+berlin".into()]).as_deref(),
            Some("café berlin")
        );
        assert_eq!(
            launch_search(&["geo:48.1,11.5?z=12&q=Marienplatz".into()]).as_deref(),
            Some("Marienplatz")
        );
        for value in ["geo:0,0", "geo:0,0?q=", "https://example.test/?q=a", "cafe"] {
            assert_eq!(launch_search(&[value.into()]), None, "{value}");
        }
    }
    use crate::model::{Category, LatLon, Place, TravelMode};
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
            "accept-places",
            "commit-search",
            "clear-route",
            "field-edit",
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
        app.invoke(Workspace::ACCEPT_PLACES.with(vec![place]))
            .unwrap();
        app.invoke(Invoke::edit(&Workspace::save("marienplatz", true)).unwrap())
            .unwrap();
        assert!(app.state().places.iter().any(|place| place.saved));
        app.harness().relayout();
        let row = actions_on(&mut app, "maps.place:marienplatz");
        for id in ["open-place", "directions"] {
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
        assert_eq!(
            app.state(),
            Workspace::default(),
            "the shared map never held the area"
        );
    }

    #[test]
    fn painted_travel_modes_swipe_through_the_canonical_document_action() {
        use winit::event::TouchPhase::{Cancelled, Ended, Started};
        fn bounds(node: &ui::headless::InspectionNode, target: &str) -> Option<vello::kurbo::Rect> {
            fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<vello::kurbo::Rect> {
                if node.action_target.as_deref() == Some(target) {
                    return node.bounds.map(ui::a11y::AccessBounds::to_rect);
                }
                node.children.iter().find_map(|child| find(child, target))
            }
            node.semantics
                .as_ref()
                .and_then(|node| find(node, target))
                .or_else(|| node.children.iter().find_map(|child| bounds(child, target)))
        }
        let mut app = TestApp::<MapsShell>::capture(Some("route")).unwrap();
        app.settle();
        assert_eq!(app.state().mode, TravelMode::Car);
        let start = bounds(&app.harness().inspection().root, "maps.mode:transit")
            .unwrap()
            .center();
        app.harness().touch(1, Started, start.x, start.y);
        app.harness().touch(1, Ended, start.x - 90.0, start.y);
        app.settle();
        assert_eq!(app.state().mode, TravelMode::Transit);
        let start = bounds(&app.harness().inspection().root, "maps.mode:transit")
            .unwrap()
            .center();
        app.harness().touch(2, Started, start.x, start.y);
        app.harness().touch(2, Cancelled, start.x - 90.0, start.y);
        app.settle();
        assert_eq!(app.state().mode, TravelMode::Transit);
        app.harness().touch(3, Started, start.x, start.y);
        app.harness().touch(3, Ended, start.x + 90.0, start.y);
        app.settle();
        assert_eq!(app.state().mode, TravelMode::Car);
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
            if crate::scenario::Sample::of(Some(scenario)) == crate::scenario::Sample::Empty {
                assert!(state.places.is_empty(), "`{scenario}` is a first run");
                assert!(state.route().is_none(), "`{scenario}` plans nothing");
                continue;
            }
            assert_eq!(
                by_id(&state.places),
                by_id(&sample.places),
                "`{scenario}` holds the city"
            );
            assert_eq!(state.recents, sample.recents, "`{scenario}` searched");
            let staged = crate::scenario::Scenario::of(Some(scenario));
            if staged.is_transit() {
                assert_eq!(state.mode, TravelMode::Transit);
                assert_eq!(
                    state.transit().len(),
                    sample.transit.len(),
                    "`{scenario}` planned transit"
                );
            } else if staged.has_route() {
                assert_eq!(
                    state.route().map(|route| route.geometry.len()),
                    sample.route.as_ref().map(|route| route.geometry.len()),
                    "`{scenario}` routes"
                );
                assert_eq!(state.mode, sample.mode);
                assert_eq!(
                    state.endpoints().map(|(_, _, _, dest)| dest.to_owned()),
                    Some(sample.dest_name.clone())
                );
            } else {
                assert!(state.route().is_none(), "`{scenario}` plans no route");
            }
        }
        let arrive = TestApp::<MapsShell>::capture(Some("transit-arrive"))
            .unwrap()
            .state()
            .transit_time;
        assert_eq!(arrive.offset_min, 60);
        assert!(arrive.arrive_by());
    }

    /// An agent saves a place, chooses how directions travel and clears a
    /// route, one call each, and reads what it did as the map's sentences.
    #[test]
    fn an_agent_works_the_map_one_call_each_and_history_reads_it() {
        use ui::tilo_actions::{ActionInvoker, ActorKind};

        let mut app = TestApp::<MapsShell>::capture(Some("route")).unwrap();
        let agent = ActionInvoker::actor("agent:claude")
            .with_kind(ActorKind::Agent)
            .acting_for("ada");
        let kind = <Workspace as tilo_resource::Resource>::KIND;
        let place = app.state().places[0].id.clone();
        let paths = Workspace::paths();
        for edit in [
            Workspace::save(&place, true),
            paths.mode.set(TravelMode::Bike),
            paths.transit_time.set(crate::model::TransitTime {
                mode: crate::model::TransitTimeMode::Arrive,
                offset_min: 30,
            }),
        ] {
            app.call_by(
                agent.clone(),
                kind,
                "field-edit",
                serde_json::json!({ "edit": edit }),
            )
            .unwrap();
        }
        for (action, args) in [
            ("commit-search", serde_json::json!({"query": "Museum"})),
            ("clear-route", serde_json::json!({})),
            ("clear-recents", serde_json::json!({})),
        ] {
            app.call_by(agent.clone(), kind, action, args).unwrap();
        }
        let state = app.state();
        assert!(state.place(&place).unwrap().saved);
        assert_eq!(state.mode, TravelMode::Bike);
        assert!(state.route().is_none());
        assert!(state.recents.is_empty());
        assert!(
            app.call_by(agent, kind, "clear-route", serde_json::json!({}))
                .is_err(),
            "there is no route left to clear"
        );

        let name = state.place(&place).unwrap().name.clone();
        let sentences = app.sentences(app.history().iter().filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        }));
        assert_eq!(
            sentences,
            [
                format!("Added {name} to the saved places"),
                "Set directions to travel by bike".to_owned(),
                "Changed when transit journeys leave or arrive".to_owned(),
                "Searched for Museum".to_owned(),
                "Cleared the route".to_owned(),
                "Cleared the recent searches".to_owned(),
            ]
        );
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
        for scenario in super::MapsShell::scenarios() {
            let mut app = TestApp::<super::MapsShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
