//! The running route as the shell's live activity: a card or a pill the
//! person keeps in reach while they work in another workspace, and the
//! control that ends the trip there.
//!
//! The shell decides whether the route's card stands on the desk: while
//! Maps' own window is on screen the directions are drawn there, and the
//! card waits. What this module publishes is the reading the pill and the
//! card's fallback show — the turn ahead, how far it is, and how long the
//! whole trip takes.

use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};

use crate::geo;
use crate::model::Route;

/// The activity id a running route is published under, and the presentation
/// the route's card is asked for with.
pub(crate) const ACTIVITY: &str = "maps.active-navigation";

/// What Maps publishes now: the running route, or nothing at all.
#[must_use]
pub(crate) fn published(route: Option<&Route>) -> Published {
    let Some(route) = route else {
        return Published::default();
    };
    let mut published = Published::of(summary(route));
    // Directions ask for their card without the person pulling it: they are
    // on their way, and the next turn should be in reach wherever they work.
    published.overlay = true;
    published
}

/// The route's reading: the turn ahead and how far it is, then how long the
/// trip still takes and how much of it is left.
fn summary(route: &Route) -> Summary {
    let next = route.steps.first();
    let (instruction, icon, distance) = next.map_or_else(
        || {
            (
                crate::failure::t("maps-continue-on-route"),
                ui::icons::Icon::ArrowUp,
                route.distance_m,
            )
        },
        |step| {
            (
                step.instruction.clone(),
                step.maneuver.icon(),
                step.distance_m,
            )
        },
    );
    let remaining = crate::failure::t_args(
        "maps-remaining",
        &[("distance", geo::fmt_distance(route.distance_m).into())],
    );
    Summary::new(Kind::Navigation, instruction, geo::fmt_distance(distance))
        .with_id(ACTIVITY)
        .with_icon(icon.name())
        .with_subtitle(format!(
            "{}, {remaining}",
            geo::fmt_duration(route.duration_s)
        ))
        // The trip is ended by the person, which the card offers as their
        // close.
        .with_control(Control::new(
            "end",
            crate::failure::t("maps-end"),
            ControlKind::Close,
        ))
}

/// A control pressed on the route's card or pill, where the person is not in
/// Maps' own window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// End the trip and clear the route.
    End,
}

/// The controls pressed on the route since the last call of this, as what
/// they ask.
#[must_use]
pub(crate) fn take_commands() -> Vec<Command> {
    ui::collaboration::channel_live_activity::take_controls()
        .into_iter()
        .filter_map(|control| {
            let command = control.get("command")?.as_str()?;
            match command {
                "end" => Some(Command::End),
                _ => None,
            }
        })
        .collect()
}

/// Show what Maps shows now as its live activity, or end it.
pub(crate) fn publish(published: &Published) {
    let shown = (published.summary.is_some())
        .then(|| serde_json::to_value(published).ok())
        .flatten();
    ui::collaboration::channel_live_activity::show(shown);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LatLon, Maneuver, RouteStep, TravelMode};

    fn route() -> Route {
        Route {
            mode: TravelMode::Car,
            distance_m: 4200.0,
            duration_s: 720.0,
            geometry: Vec::new(),
            steps: vec![
                RouteStep {
                    instruction: "Turn right onto 9th Street".into(),
                    distance_m: 180.0,
                    maneuver: Maneuver::Right,
                },
                RouteStep {
                    instruction: "Arrive at 4th Avenue".into(),
                    distance_m: 0.0,
                    maneuver: Maneuver::Arrive,
                },
            ],
            from: LatLon {
                lat: 37.7,
                lon: -122.4,
            },
            to: LatLon {
                lat: 37.8,
                lon: -122.4,
            },
        }
    }

    /// The route reads as the turn ahead: what to do, how far it is, and how
    /// long the whole trip takes with how much is left of it.
    #[test]
    fn the_route_reads_as_the_turn_ahead_and_the_trip_left() {
        let route = route();
        let published = published(Some(&route));
        assert!(published.overlay, "directions ask for their card");
        let summary = published.summary.expect("a summary");
        assert_eq!(summary.kind, Kind::Navigation);
        assert_eq!(summary.id, ACTIVITY);
        assert_eq!(summary.title, "Turn right onto 9th Street");
        assert_eq!(summary.value, "180 m", "the distance to the turn");
        assert_eq!(summary.icon, "corner-up-right");
        assert!(
            summary.subtitle.contains("12 min") && summary.subtitle.contains("4.2 km"),
            "the trip left: {}",
            summary.subtitle
        );
        let close = summary.close_control().expect("a close");
        assert_eq!(close.id, "end");
    }

    /// With no route there is nothing to show.
    #[test]
    fn no_route_is_no_activity() {
        let published = published(None);
        assert!(published.summary.is_none());
        assert!(!published.overlay);
    }
}
