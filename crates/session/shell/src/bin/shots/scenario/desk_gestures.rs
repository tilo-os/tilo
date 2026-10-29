//! Films of the desk widgets under the hand, for review: a glance tapped
//! open and folded back by Escape, a widget carried and thrown with
//! momentum, a card resized through every breakpoint, and a widget removed
//! from its hovered badge. Every frame at 60 Hz (needs the shell's
//! `test-support` feature, see [`super::film`]).

use shell::shell::desk_widgets::WidgetId;
use winit::keyboard::NamedKey;

use super::Action;
use super::film::{Film, Step};
use super::motion_review::{NONE, named, rest, scenario, step};

fn spot(widget: WidgetId, spot: &'static str) -> Step {
    Step::DeskSpot { widget, spot }
}

fn press() -> Step {
    Step::Act(Box::new(Action::MousePress))
}

fn release() -> Step {
    Step::Act(Box::new(Action::MouseRelease))
}

/// A hand moving `steps` frames from the last spot to `(dx, dy)` from it,
/// starting at frame `from`, easing out as a hand slowing down does when
/// `ease`, at an even pace (a throw) otherwise.
fn path(mut film: Film, from: u32, steps: u32, (dx, dy): (f64, f64), ease: bool) -> Film {
    for k in 1..=steps {
        let t = f64::from(k) / f64::from(steps);
        let t = if ease { 1.0 - (1.0 - t) * (1.0 - t) } else { t };
        film = film.then_at(
            from.saturating_add(k),
            Step::DeskFrom {
                dx: dx * t,
                dy: dy * t,
            },
        );
    }
    film
}

pub(super) fn all() -> super::Scenario {
    let mut a = vec![
        Action::SeedDesk,
        step(Step::AddDeskWidget(WidgetId::Calendar)),
        Action::Wait(10),
    ];
    // Tapped open in place, folded back by Escape.
    rest(&mut a);
    a.push(Action::Film(Film::new(
        "01-music-tap-expands",
        vec![spot(WidgetId::Music, "expand"), press(), release()],
    )));
    a.push(Action::Film(Film::new(
        "02-escape-folds",
        vec![Step::Key(NONE, named(NamedKey::Escape))],
    )));
    // Carried toward the middle and set down, then thrown to the left.
    rest(&mut a);
    let carry = Film::new(
        "03-music-carried-and-set-down",
        vec![spot(WidgetId::Music, "glance"), press()],
    );
    let carry = path(carry, 0, 40, (-620.0, -330.0), true);
    let carry = carry.then_at(56, release()).at_least(90);
    a.push(Action::Film(carry));
    rest(&mut a);
    let throw = Film::new(
        "04-music-thrown-left",
        vec![spot(WidgetId::Music, "glance"), press()],
    );
    let throw = path(throw, 0, 8, (-360.0, 40.0), false);
    let throw = throw.then_at(9, release()).at_least(70);
    a.push(Action::Film(throw));
    // The calendar opened, then widened through its breakpoints and back
    // narrower.
    rest(&mut a);
    a.push(Action::Film(Film::new(
        "05-calendar-tap-expands",
        vec![spot(WidgetId::Calendar, "glance"), press(), release()],
    )));
    rest(&mut a);
    let resize = Film::new(
        "06-calendar-resized",
        vec![spot(WidgetId::Calendar, "grip"), press()],
    );
    let resize = path(resize, 0, 60, (-160.0, 0.0), true);
    let resize = path(resize, 60, 60, (90.0, 0.0), true);
    let resize = resize.then_at(124, release()).at_least(130);
    a.push(Action::Film(resize));
    a.push(Action::Key(winit::keyboard::Key::Named(NamedKey::Escape)));
    // Hovered, the music glance shows its remove badge; pressed, it goes
    // and Undo is offered.
    rest(&mut a);
    a.push(Action::Film(
        Film::new(
            "07-music-hover-shows-remove",
            vec![spot(WidgetId::Music, "glance")],
        )
        .at_least(20),
    ));
    a.push(Action::Film(Film::new(
        "08-music-removed",
        vec![spot(WidgetId::Music, "badge"), press(), release()],
    )));
    scenario(
        "motion_review_desk_widgets",
        "Desk widgets under the hand: a glance tapped open in place and folded by Escape, a widget carried and set down, a widget thrown with momentum, a card resized through its breakpoints, a widget removed from its hovered badge with Undo offered. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}
