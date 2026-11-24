//! Who is at a place on a map: another member's or agent's lens on it docks their face on the
//! place's marker, as a list row wears the member at its item, and this person's own view shows
//! none of its own.

use super::*;
use crate::Theme;
use crate::component::Person;
use crate::draw::DrawCtx;
use crate::permissions::MemberId;
use crate::presence::PresenceCtx;

/// The agent whose view stands at the place.
const SCOUT: &str = "scout";

/// The colour the presence paints the agent in, which no theme's marker inks are close to.
const SCOUT_INK: (u8, u8, u8) = (250, 0, 250);

/// Where the tested place is: a café in Munich.
const PLACE: (f64, f64) = (48.137, 11.575);

/// The agent's lens on a place, as a view hands it over: `lenses(place).find(l -> not l.mine)?.who`.
fn scout() -> Person {
    Person::new(MemberId::agent(SCOUT), "Scout")
}

/// Presence with the agent in it, coloured [`SCOUT_INK`], heard from another window.
fn presence() -> PresenceCtx {
    let mut presence = PresenceCtx::default();
    presence.set_self_member(MemberId::person("me"), "Me");
    presence
        .names
        .insert(MemberId::agent(SCOUT), "Scout".to_owned());
    presence
        .color_overrides
        .insert(MemberId::agent(SCOUT), SCOUT_INK);
    presence
}

/// A map of one place, looked at so its marker stands at the viewport's middle, with `at_the_place`
/// who is there.
fn map(at_the_place: Option<Person>) -> Map {
    let mut map = Map::new();
    let projected = mercator_project(PLACE.0, PLACE.1);
    map.set_camera(MapCamera {
        origin: projected - Vec2::new(400.0, 300.0),
        zoom: 1.0,
    });
    map.markers = vec![(
        "cafe".to_owned(),
        PLACE,
        MapMarkerKind::Cafe,
        false,
        Some("Café".to_owned()),
        at_the_place,
        false,
    )];
    map
}

/// Draw `map` over an 800 by 600 viewport with `presence` lent, answering its pixels as RGBA.
fn drawn(map: &mut Map, presence: &PresenceCtx) -> Vec<u8> {
    let theme = Theme::dark();
    let mut text = crate::font_catalog::fixture_renderer();
    crate::widget::layout::lay_out_detached(
        map,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        &theme,
        &mut text,
    )
    .expect("the map lays out");
    let env = presence.draw_env();
    let mut scene = Scene::new();
    map.draw(&mut DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
        env,
    ));
    let mut pixels = vec![0_u8; 800 * 600 * 4];
    let mut target =
        tilo_cpu_raster::Target::new(&mut pixels, 800, 600, 800 * 4).expect("a raster target");
    tilo_cpu_raster::CpuRasterizer::new(tilo_cpu_raster::Options::default()).render(
        scene.encoding(),
        &mut target,
        None,
        vello::peniko::Color::TRANSPARENT,
    );
    pixels
}

/// The pixels of `pixels` (RGBA, 800 wide) painted in the agent's ink, within a step of it: the
/// colour the theme paints their presence in, which is what the tag's pill wears.
fn scout_pixels(pixels: &[u8]) -> Vec<(u32, u32)> {
    let ink = Theme::dark()
        .tokens
        .colors
        .presence
        .of_rgb(SCOUT_INK)
        .to_rgba8()
        .to_u8_array();
    let close = |channel: usize, want: u8| pixels[channel].abs_diff(want) <= 2;
    let mut found = Vec::new();
    for at in 0..(pixels.len() / 4) {
        let px = at * 4;
        if pixels[px + 3] > 200
            && close(px, ink[0])
            && close(px + 1, ink[1])
            && close(px + 2, ink[2])
        {
            let at = u32::try_from(at).unwrap_or_default();
            found.push((at % 800, at / 800));
        }
    }
    found
}

/// Another window at a place docks its person's face on the place's marker, right of it and at
/// its height.
#[test]
fn a_lens_on_a_place_docks_on_its_marker() {
    let here = presence();
    let mut with = map(Some(scout()));
    let painted = scout_pixels(&drawn(&mut with, &here));
    assert!(
        painted.len() > 40,
        "the agent's tag paints in their colour: {} pixels",
        painted.len()
    );
    let middle = 400.0;
    let right: f64 = painted.iter().map(|(x, _)| f64::from(*x)).sum::<f64>() / painted.len() as f64;
    assert!(
        right > middle + MARKER_RADIUS,
        "it docks to the right of the marker: its middle at {right}"
    );
    let above: f64 = painted.iter().map(|(_, y)| f64::from(*y)).sum::<f64>() / painted.len() as f64;
    assert!(
        (above - 300.0).abs() < 30.0,
        "it stands at the marker's height: its middle at {above}"
    );
}

/// A place nobody else's view stands at shows no face, and neither does this person's own view
/// of one.
#[test]
fn no_lens_and_ones_own_view_dock_nobody() {
    let here = presence();
    let mut empty = map(None);
    assert!(
        scout_pixels(&drawn(&mut empty, &here)).is_empty(),
        "a place no one else is at shows no face"
    );
    let mut own = presence();
    own.set_own_identity([MemberId::agent(SCOUT)]);
    let mut mine = map(Some(scout()));
    assert!(
        scout_pixels(&drawn(&mut mine, &own)).is_empty(),
        "a person's own view is not someone else at the place"
    );
}
