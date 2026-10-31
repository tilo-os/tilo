use std::sync::Arc;

use tilo_theme_sdk::kurbo::{Rect, Shape as _};
use tilo_theme_sdk::wire::{self, PaintRequest, Request};
use tilo_theme_sdk::{Op, Param};

use super::*;

fn tokens(params: &[(&str, f32)]) -> PartTokens {
    PartTokens {
        fill: Color::from_rgb8(0xd0, 0x30, 0x20),
        ink: Color::WHITE,
        accent: Color::from_rgb8(0x20, 0x40, 0xc0),
        params: params
            .iter()
            .map(|&(name, value)| Param {
                name: name.to_owned(),
                value,
            })
            .collect::<Vec<_>>()
            .into(),
        ..PartTokens::default()
    }
}

fn paint(part: Part, state: PartState, progress: f32, tokens: &PartTokens) -> PaintedPart {
    Bauhaus
        .paint(part, Size::new(120.0, 40.0), state, progress, tokens)
        .expect("Bauhaus draws this part")
}

fn patterns(scene: &PartScene) -> usize {
    scene
        .ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                Op::Fill {
                    brush: Brush::Pattern { .. },
                    ..
                }
            )
        })
        .count()
}

fn face(painted: &PaintedPart) -> Rect {
    painted
        .scene
        .ops
        .iter()
        .find_map(|op| match op {
            Op::Fill {
                path,
                brush: Brush::Solid(_),
                ..
            } => Some(path.bounding_box()),
            _ => None,
        })
        .expect("a solid face")
}

/// The declared parameters are ones the toolkit admits: they survive the
/// wire's checks a loaded module's answer goes through.
#[test]
fn the_parameters_cross_the_wire_intact() {
    let params = Bauhaus.params();
    assert_eq!(wire::params(&wire::params_answer(&params)), Some(params));
}

/// A floating panel's foot is shaded in one dithered band per level past
/// the first, so `levels` is what the document sees change.
#[test]
fn levels_set_how_many_bands_the_shade_takes() {
    let rest = PartState::settled(States::REST);
    for (levels, bands) in [(2.0, 1), (4.0, 3), (9.0, 8)] {
        let painted = paint(
            Part::FloatingPanel,
            rest,
            1.0,
            &tokens(&[("levels", levels)]),
        );
        assert_eq!(patterns(&painted.scene), bands);
    }
    let flat = paint(Part::FloatingPanel, rest, 1.0, &tokens(&[("depth", 0.0)]));
    assert_eq!(patterns(&flat.scene), 0, "no depth, no shade");
}

/// A pressed button's face sinks onto its shadow, in the steps its
/// transition runs, and its rect stays where the pointer lands.
#[test]
fn a_pressed_button_sinks_onto_its_shadow() {
    let t = tokens(&[("shadow", 4.0)]);
    let rest = face(&paint(
        Part::ButtonContainer,
        PartState::settled(States::REST),
        1.0,
        &t,
    ));
    assert_eq!(rest, Rect::new(0.0, 0.0, 116.0, 36.0));
    let pressing = PartState {
        now: States::PRESSED,
        was: States::REST,
    };
    let half = face(&paint(Part::ButtonContainer, pressing, 0.5, &t));
    assert_eq!(half, Rect::new(2.0, 2.0, 118.0, 38.0));
    let down = face(&paint(
        Part::ButtonContainer,
        PartState::settled(States::PRESSED),
        1.0,
        &t,
    ));
    assert_eq!(down, Rect::new(4.0, 4.0, 120.0, 40.0));
    assert!(matches!(
        Bauhaus.transition(Part::ButtonContainer, States::REST, States::PRESSED),
        Transition::Stepped { steps: 3, .. }
    ));
    let plain = PartTokens {
        fill: Color::TRANSPARENT,
        ..t
    };
    assert!(
        paint(
            Part::ButtonContainer,
            PartState::settled(States::REST),
            1.0,
            &plain
        )
        .scene
        .ops
        .is_empty(),
        "a plain button has no face to stand on a shadow"
    );
}

/// A tile turns over `amount` of its texels, evenly.
#[test]
fn a_dither_tile_turns_over_its_share_of_texels() {
    let black = Color::BLACK.to_rgba8();
    for (amount, dark) in [(0.0, 0), (0.25, 4), (0.5, 8), (1.0, 16)] {
        let tile = dither(Color::WHITE, Color::BLACK, amount);
        assert!(tile.is_well_formed());
        let turned = tile
            .rgba
            .chunks(4)
            .filter(|texel| texel[0] == black.r)
            .count();
        assert_eq!(turned, dark, "amount {amount}");
    }
    // Half: no two turned texels share an edge, a checkerboard.
    let half = dither(Color::WHITE, Color::BLACK, 0.5);
    let at = |x: usize, y: usize| half.rgba[(y % 4 * 4 + x % 4) * 4] == black.r;
    for y in 0..4 {
        for x in 0..4 {
            assert_ne!(at(x, y), at(x + 1, y));
        }
    }
}

/// The module's one entry point answers what the host asks, through the
/// same bytes it would cross in wasm.
#[test]
fn the_exported_entry_answers_the_host() {
    let ask = |request: Vec<u8>| tilo_theme_sdk::guest::answer(&Bauhaus, &request);
    assert_eq!(
        wire::params(&ask(wire::params_request())),
        Some(Bauhaus.params())
    );
    let request = PaintRequest {
        part: Part::BarBody,
        size: Size::new(800.0, 32.0),
        state: PartState::settled(States::REST),
        progress: 1.0,
        tokens: PartTokens {
            params: Arc::new([]),
            ..tokens(&[])
        },
    };
    assert!(matches!(
        wire::request(&wire::paint_request(&request)),
        Some(Request::Paint(_))
    ));
    let painted = wire::paint(&ask(wire::paint_request(&request)))
        .flatten()
        .expect("a bar body");
    assert_eq!(
        painted,
        Bauhaus
            .paint(
                request.part,
                request.size,
                request.state,
                1.0,
                &request.tokens
            )
            .unwrap()
    );
}
