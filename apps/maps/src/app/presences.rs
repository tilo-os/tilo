//! Who else is at a place: another view of this window (an agent's) left at a place is shown
//! docked on its pin, as the toolkit shows members an app placed, and followed when this person
//! follows them.

use ui::framework::Landing;
use ui::paint::PaintCtx;
use ui::presence::{PeerCursor, PlacedPointer, PresenceCtx};
use ui::tilo_actions::MemberId;

use super::{MapPalette, MapsApp, Point, Rect, Scene, TextRenderer, Theme, canvas};
use crate::model::LatLon;
use vello::kurbo::Affine;

/// The members other views left at one place, and where its pin docks them.
#[derive(Debug)]
pub(crate) struct AtPlace {
    /// Where the first face's centre docks on the pin.
    pub(crate) dock: Point,
    /// The middle of the pin's head.
    pub(crate) head: Point,
    pub(crate) members: Vec<(MemberId, Option<String>)>,
}

impl MapsApp {
    /// Where the place a row names stands: a place, a search answer or a point of interest the
    /// map knows.
    fn position_of(&self, row: &str) -> Option<LatLon> {
        let id = super::window::entity_id(row);
        self.model
            .place(id)
            .or_else(|| self.model.results.iter().find(|place| place.id == id))
            .or_else(|| self.place_search.geocoded(id))
            .map(|place| place.pos)
            .or_else(|| self.pois.iter().find(|poi| poi.id == id).map(|poi| poi.pos))
    }

    /// The members other views left at places, by place, in the order they arrived there.
    /// This person's own views, and places the map does not know, show nobody.
    pub(crate) fn at_places(
        &self,
        landings: &[Landing],
        presence: &PresenceCtx,
        theme: &Theme,
    ) -> Vec<AtPlace> {
        let proj = self.projection();
        let scale = MapPalette::from_theme(theme).marker_scale;
        let mut places: Vec<(&str, AtPlace)> = Vec::new();
        for landing in landings {
            let Some(member) = landing.member.clone() else {
                continue;
            };
            let ui::tilo_resource::Place::Item(row) = &landing.place else {
                continue;
            };
            if presence.is_own_identity(&member) {
                continue;
            }
            let id = super::window::entity_id(row);
            let shown = (member, landing.name.clone());
            if let Some((_, at)) = places.iter_mut().find(|(place, _)| *place == id) {
                at.members.push(shown);
                continue;
            }
            let Some(pos) = self.position_of(row) else {
                continue;
            };
            let anchor = proj.project(pos);
            let selected = self.model.selected.as_deref() == Some(id);
            places.push((
                id,
                AtPlace {
                    dock: canvas::pin_dock(anchor, selected, scale),
                    head: canvas::pin_head(anchor, selected, scale),
                    members: vec![shown],
                },
            ));
        }
        places.into_iter().map(|(_, at)| at).collect()
    }

    /// Draw [`Self::at_places`] over the map: docked on the pin where it shows, else as the edge
    /// mist that points to it. The member this person follows wears the followed ring. Answers
    /// where it painted.
    pub(crate) fn draw_presences(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (presence, landings, following): (&PresenceCtx, &[Landing], Option<&MemberId>),
    ) -> Vec<Rect> {
        let ground = MapPalette::from_theme(theme).land;
        let mut painted = Vec::new();
        let mut away: Vec<PeerCursor> = Vec::new();
        for at in self.at_places(landings, presence, theme) {
            if !self.rect.contains(at.head) {
                away.extend(at.members.iter().map(|(member, name)| {
                    presence.placed_cursor(PlacedPointer {
                        user: member.clone(),
                        at: (at.head.x, at.head.y),
                        name: name.clone(),
                        activity: None,
                        chat: None,
                        reaction: None,
                    })
                }));
                continue;
            }
            let followed = at
                .members
                .first()
                .is_some_and(|(member, _)| Some(member) == following);
            painted.push(presence.draw_placed_tag(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                (at.dock, self.rect),
                &at.members,
                ground,
                followed,
            ));
        }
        painted.extend(ui::presence::draw_peer_presence(
            scene,
            transform,
            self.rect,
            text_renderer,
            theme,
            &away,
        ));
        painted
    }

    /// Show the place a followed member's view went to.
    pub(crate) fn show_followed(&mut self, row: &str) {
        if let Some(pos) = self.position_of(row) {
            self.center_on(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use ui::collaboration::CollabEvent;
    use ui::framework::testing::TestApp;
    use ui::presence::PresenceCtx;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use ui::tilo_actions::{ActionInvoker, MemberId};

    use super::super::MapWindow;
    use crate::MapsShell;
    use crate::canvas::{self, MapPalette};
    use crate::model::{Category, LatLon, Place};
    use crate::workspace::Workspace;

    fn map() -> TestApp<MapsShell> {
        let mut app = TestApp::<MapsShell>::new().unwrap();
        let place = |id: &str, name: &str, lat: f64, lon: f64| {
            let mut place = Place::new(id, name, Category::Cafe, "Munich", LatLon::new(lat, lon));
            place.saved = true;
            place
        };
        app.invoke(Workspace::ACCEPT_PLACES.with(vec![
            place("cafe-glueck", "Café Glück", 48.137, 11.575),
            place("kaffee-rösterei", "Kaffeerösterei", 48.139, 11.571),
            place("brezn", "Brezn", 48.135, 11.579),
        ]))
        .unwrap();
        app.settle();
        app
    }

    /// An agent opens `place` by the row the map publishes for it: a screen
    /// effect is offered on that row, and places its invoker there.
    fn open(app: &mut TestApp<MapsShell>, invoker: ActionInvoker, place: &str) {
        let row = format!("maps.place:{place}");
        app.call_by(
            invoker,
            &row,
            "open-place",
            serde_json::json!({ "place": row }),
        )
        .unwrap();
        app.settle();
    }

    fn scout() -> ActionInvoker {
        ActionInvoker::agent("scout").named(Some("Scout".into()))
    }

    fn guide() -> ActionInvoker {
        ActionInvoker::agent("guide").named(Some("Guide".into()))
    }

    /// Where `place`'s pin has its head, and where it docks a face, on the map now.
    fn pin(
        window: &MapWindow,
        place: &str,
        theme: &Theme,
    ) -> (vello::kurbo::Point, vello::kurbo::Point) {
        let app = window.app.borrow();
        let anchor = app
            .projection()
            .project(app.model.place(place).unwrap().pos);
        let scale = MapPalette::from_theme(theme).marker_scale;
        (
            canvas::pin_head(anchor, false, scale),
            canvas::pin_dock(anchor, false, scale),
        )
    }

    fn members(at: &super::AtPlace) -> Vec<MemberId> {
        at.members
            .iter()
            .map(|(member, _)| member.clone())
            .collect()
    }

    #[test]
    fn an_agent_at_a_place_docks_on_its_pin() {
        let mut app = map();
        open(&mut app, scout(), "cafe-glueck");
        let theme = Theme::light();
        let (at, (head, dock), painted) = app
            .with_surface(|window: &mut MapWindow| {
                let landings = window.live.landings();
                let presence = PresenceCtx::empty();
                let app = window.app.borrow();
                let at = app.at_places(&landings, &presence, &theme);
                let painted = app.draw_presences(
                    &mut vello::Scene::new(),
                    vello::kurbo::Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &theme,
                    (&presence, &landings, None),
                );
                drop(app);
                (at, pin(window, "cafe-glueck", &theme), painted)
            })
            .unwrap();
        let [at] = at.as_slice() else {
            panic!("one place has someone at it: {at:?}");
        };
        assert_eq!(members(at), [MemberId::agent("scout")]);
        assert_eq!(at.members[0].1.as_deref(), Some("Scout"));
        assert_eq!(at.dock, dock, "the agent docks on the pin");
        assert!(
            dock.x > head.x && dock.y < head.y,
            "above and right of the pin's head, which shows whole: {dock:?} {head:?}"
        );
        let [painted] = painted.as_slice() else {
            panic!("one tag is painted: {painted:?}");
        };
        assert!(
            painted.contains(dock),
            "the tag is painted at the pin: {painted:?} {dock:?}"
        );
        assert_eq!(
            app.with_surface(|window: &mut MapWindow| window.app.borrow().model.selected.clone()),
            Some(None),
            "the person's view stays"
        );
    }

    #[test]
    fn agents_at_two_places_dock_on_their_own_pins_and_at_one_share_its_tag() {
        let mut app = map();
        open(&mut app, scout(), "cafe-glueck");
        open(&mut app, guide(), "brezn");
        let theme = Theme::dark();
        let shown = |app: &mut TestApp<MapsShell>| {
            app.with_surface(|window: &mut MapWindow| {
                let landings = window.live.landings();
                let at = window
                    .app
                    .borrow()
                    .at_places(&landings, &PresenceCtx::empty(), &theme);
                (
                    at,
                    pin(window, "cafe-glueck", &theme).1,
                    pin(window, "brezn", &theme).1,
                )
            })
            .unwrap()
        };
        let (at, cafe, brezn) = shown(&mut app);
        let dock_of = |id: &str| {
            at.iter()
                .find(|at| members(at) == [MemberId::agent(id)])
                .map(|at| at.dock)
        };
        assert_eq!(dock_of("scout"), Some(cafe));
        assert_eq!(dock_of("guide"), Some(brezn));
        open(&mut app, guide(), "cafe-glueck");
        let (at, cafe, _) = shown(&mut app);
        let [at] = at.as_slice() else {
            panic!("both are at one place: {at:?}");
        };
        assert_eq!(at.dock, cafe);
        assert_eq!(
            members(at).len(),
            2,
            "one tag names both: {:?}",
            members(at)
        );
    }

    #[test]
    fn following_an_agent_shows_the_places_its_view_goes_to() {
        let mut app = map();
        open(&mut app, scout(), "cafe-glueck");
        let centre = |app: &mut TestApp<MapsShell>| {
            app.with_surface(|window: &mut MapWindow| window.app.borrow().model.center)
                .unwrap()
        };
        let before = centre(&mut app);
        app.harness()
            .handle_collaboration_event(&CollabEvent::FollowChanged {
                peer_id: Some(MemberId::agent("scout")),
            });
        app.settle();
        let shown_at = |app: &mut TestApp<MapsShell>, place: &str| {
            app.with_surface(|window: &mut MapWindow| {
                let app = window.app.borrow();
                let pos = app.model.place(place).unwrap().pos;
                let shown = app.projection().project(pos);
                let middle = app.visible_map_area().center();
                (shown - middle).hypot() < 1.0
            })
            .unwrap()
        };
        assert!(
            shown_at(&mut app, "cafe-glueck"),
            "following shows where the agent is"
        );
        open(&mut app, scout(), "kaffee-rösterei");
        assert!(
            shown_at(&mut app, "kaffee-rösterei"),
            "and where it goes next"
        );
        assert_eq!(
            app.with_surface(|window: &mut MapWindow| window.app.borrow().model.selected.clone()),
            Some(None),
            "following moves the map, never what this person opened"
        );
        app.harness()
            .handle_collaboration_event(&CollabEvent::FollowChanged { peer_id: None });
        let stopped = centre(&mut app);
        open(&mut app, scout(), "brezn");
        assert_eq!(centre(&mut app), stopped, "stopped, the map stays");
        assert_ne!(stopped, before);
    }
}
