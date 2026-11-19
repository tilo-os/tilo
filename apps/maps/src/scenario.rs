//! The screenshot scenarios: the bundled sample city, staged. A capture never reaches the network;
//! places and plans are written with the same actions a person's edits take, and the view state is
//! staged once they are laid out.

use ui::framework::Fixture;
use ui::tilo_resource::Invoke;

use crate::model::{TransitTime, TransitTimeMode, TravelMode};
use crate::sample;
use crate::workspace::{Workspace, workspace};

/// Every named scenario `--shots --scenario NAME` captures; a `-narrow` one is a phone-sized
/// window, where the panel is a drawer.
pub(crate) const SCENARIOS: &[&str] = &[
    "directions",
    "explore",
    "agent",
    "agents",
    "agents-together",
    "follow",
    "search",
    "pois",
    "transit",
    "transit-arrive",
    "edit-origin",
    "traffic",
    "traffic-empty",
    "offline",
    "menu",
    "attribution",
    "error",
    "collapsed",
    "navigation",
    "floating-navigation",
    "floating-navigation-attached",
    "floating-navigation-hover",
    "floating-navigation-returning",
    "floating-navigation-returned",
    "empty",
    "directions-narrow",
    "explore-narrow",
    "agents-narrow",
    "search-narrow",
    "pois-narrow",
    "transit-narrow",
    "offline-narrow",
    "navigation-narrow",
    "empty-narrow",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scenario {
    Directions,
    Explore,
    Search,
    Pois,
    Transit,
    TransitArrive,
    EditOrigin,
    Traffic,
    Offline,
    Menu,
    Attribution,
    Error,
    Collapsed,
    Navigation,
    FloatingNavigation,
    Empty,
}

impl Scenario {
    pub(crate) fn of(name: Option<&str>) -> Self {
        let name = name.unwrap_or_default();
        match name.strip_suffix("-narrow").unwrap_or(name) {
            "explore" | "agent" | "agents" | "agents-together" | "follow" => Self::Explore,
            "search" => Self::Search,
            "pois" => Self::Pois,
            "transit" => Self::Transit,
            "transit-arrive" => Self::TransitArrive,
            "edit-origin" => Self::EditOrigin,
            "traffic" | "traffic-empty" => Self::Traffic,
            "offline" => Self::Offline,
            "menu" => Self::Menu,
            "attribution" => Self::Attribution,
            "error" => Self::Error,
            "collapsed" => Self::Collapsed,
            "navigation" => Self::Navigation,
            "empty" => Self::Empty,
            name if name.starts_with("floating-navigation") => Self::FloatingNavigation,
            _ => Self::Directions,
        }
    }

    pub(crate) fn has_route(self) -> bool {
        matches!(
            self,
            Self::Directions
                | Self::EditOrigin
                | Self::Error
                | Self::Collapsed
                | Self::Navigation
                | Self::FloatingNavigation
        )
    }

    pub(crate) fn is_transit(self) -> bool {
        matches!(self, Self::Transit | Self::TransitArrive)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sample {
    City,
    Empty,
}

impl Sample {
    pub(crate) fn of(scenario: Option<&str>) -> Self {
        if scenario == Some("traffic-empty") || Scenario::of(scenario) == Scenario::Empty {
            Self::Empty
        } else {
            Self::City
        }
    }
}

pub(crate) fn prepare(fixture: &mut Fixture<Workspace>) {
    let sample = Sample::of(fixture.scenario());
    for (id, name, place) in agents(fixture.scenario()) {
        fixture.agent(
            id,
            name,
            ui::tilo_resource::Place::Item((*place).to_owned()),
        );
    }
    fixture.provide(sample);
}

pub(crate) fn viewport(scenario: Option<&str>) -> (u32, u32) {
    match scenario {
        Some(name) if name.ends_with("-narrow") => (520, 820),
        _ => (1280, 800),
    }
}

pub(crate) fn actions(name: Option<&str>) -> Vec<Invoke<Workspace>> {
    if Sample::of(name) == Sample::Empty {
        return Vec::new();
    }
    let scenario = Scenario::of(name);
    let model = sample::default_model();
    let mut actions = vec![Workspace::ACCEPT_PLACES.with(model.places.clone())];
    actions.extend(
        model
            .recents
            .iter()
            .rev()
            .map(|query| Workspace::COMMIT_SEARCH.with(query.as_str())),
    );
    let mode = if scenario.is_transit() {
        TravelMode::Transit
    } else {
        model.mode
    };
    actions.extend(Invoke::edit(&Workspace::paths().mode.set(mode)).ok());
    if scenario == Scenario::TransitArrive {
        actions.extend(
            Invoke::edit(&Workspace::paths().transit_time.set(TransitTime {
                mode: TransitTimeMode::Arrive,
                offset_min: 60,
            }))
            .ok(),
        );
    }
    if scenario.has_route()
        && let Some(route) = model.route.clone()
    {
        actions.push(Workspace::ACCEPT_ROUTE.with(workspace::AcceptRoute {
            route,
            origin: model.origin,
            origin_name: model.origin_name.clone(),
            dest: model.dest,
            dest_name: model.dest_name.clone(),
            mode: model.mode,
        }));
    }
    if scenario.is_transit() {
        actions.push(Workspace::ACCEPT_TRANSIT.with(workspace::AcceptTransit {
            transit: model.transit.clone(),
            origin: model.origin,
            origin_name: model.origin_name.clone(),
            dest: model.dest,
            dest_name: model.dest_name,
        }));
    }
    actions
}

/// The agents a scenario shows, each with the place its own view of the map opened: its id, its
/// name and the place.
fn agents(name: Option<&str>) -> &'static [(&'static str, &'static str, &'static str)] {
    match name.map(|name| name.strip_suffix("-narrow").unwrap_or(name)) {
        Some("agent" | "follow") => &[("scout", "Scout", "cafe-glueck")],
        Some("agents") => &[
            ("scout", "Scout", "cafe-glueck"),
            ("guide", "Guide", "cafe-luitpold"),
        ],
        Some("agents-together") => &[
            ("scout", "Scout", "cafe-glueck"),
            ("guide", "Guide", "cafe-glueck"),
        ],
        _ => &[],
    }
}

/// What the person does in the scene: in `follow`, they follow Scout's view, as the session
/// asks a window to.
pub(crate) fn input(name: Option<&str>, input: &mut ui::headless::FrameHarness) {
    if name == Some("follow") {
        input.handle_collaboration_event(&ui::collaboration::CollabEvent::FollowChanged {
            peer_id: Some(ui::tilo_actions::MemberId::agent("scout")),
        });
    }
}
