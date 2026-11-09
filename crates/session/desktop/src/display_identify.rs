//! Which screen is which: each display says its number and name in a corner of itself for a moment
//! after the person presses Identify in Settings.
//! Settings numbers the displays on its arrangement canvas; the same numbers and names stand on the
//! physical screens, so "2" on the canvas is the "2" the person is looking at. The card keeps out
//! of the way of the arranging: it stands in a display's bottom-left corner, never over its centre,
//! and it goes on its own after [`HOLD`], fading out on the standard confirmation's exit. Nothing
//! counts down on a timer of its own: the card is up while the desk's clock says so, and asks for
//! the frame that takes it away ([`DisplayIdentify::next_wake_deadline`]).

use ui::a11y::{AccessNode, Politeness, Role};
use ui::motion::Transient;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::tokens::TextRole;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

/// How long each display says which one it is: long enough to look from the canvas to the screens,
/// short enough not to be in the way.
pub const HOLD: Duration = Duration::from_secs(3);

pub use tilo_desktop_surface::host::chrome::{Ask, Label};

/// One card as it stands on a display, in the desk's coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    /// The whole display the card stands on.
    pub display: Rect,
    /// The card itself.
    pub rect: Rect,
    /// The number on the card.
    pub number: usize,
    /// The name beside it.
    pub name: String,
}

/// The identify cards as the chrome holds them: the request being answered,
/// when it came, and the cards once they are painted.
#[derive(Debug, Default)]
pub struct DisplayIdentify {
    /// The request answered now, and when it came.
    up: Option<(u64, Instant)>,
    /// Every display's card, as given when the request was answered.
    labels: Vec<Label>,
    /// When the cards begin to fade, as the theme read at the last paint says.
    fade_from: Option<Instant>,
    /// The cards as painted, for assistive technology; empty while there are none.
    cards: Vec<Card>,
}

impl DisplayIdentify {
    /// Answer `ask` , once per request: asking again (a second press of Identify) shows the cards
    /// afresh with the labels of that moment, and no ask at all takes them away.
    pub fn follow(&mut self, ask: Option<Ask>, now: Instant) {
        let answering = match &ask {
            Some(ask) => self.up.is_none_or(|(id, _)| id != ask.id),
            // Nothing is asked and something stands: it comes down.
            None => self.up.is_some(),
        };
        if !answering {
            // The request answered already stands: its cards do not start over on every pass, and
            // no ask leaves what is up alone.
            return;
        }
        if let Some(ask) = ask {
            self.up = Some((ask.id, now));
            self.labels = ask.labels;
        } else {
            self.up = None;
            self.labels.clear();
        }
        self.fade_from = None;
        self.cards.clear();
    }

    /// Whether a card stands anywhere at `now` .
    #[must_use]
    pub fn is_up(&self, now: Instant) -> bool {
        self.gone(now).is_some() && !self.labels.is_empty()
    }

    /// When the cards go, while they stand at `now` .
    fn gone(&self, now: Instant) -> Option<Instant> {
        let (_, at) = self.up?;
        let gone = at.checked_add(HOLD)?;
        (now < gone).then_some(gone)
    }

    /// The cards as painted, empty while none stand.
    #[must_use]
    pub fn cards(&self) -> &[Card] {
        &self.cards
    }

    /// Whether a frame is owed at `now` : the cards stand, or the last ones still stand in the
    /// scene and must be taken away.
    #[must_use]
    pub fn needs_redraw(&self, now: Instant) -> bool {
        self.is_up(now) || !self.cards.is_empty()
    }

    /// Whether the cards fade at `now` , so the frame clock owes frames.
    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        self.is_up(now)
            && self.fade_from.is_some_and(|from| {
                self.gone(now)
                    .is_some_and(|gone| from < gone && now >= from)
            })
    }

    /// When the cards next need a frame without input.
    #[must_use]
    pub fn next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        let gone = self.gone(now)?;
        Some(match self.fade_from.filter(|from| *from < gone) {
            Some(from) if now < from => from,
            _ => gone,
        })
    }

    /// What the cards say to assistive technology, while they stand at `now` .
    #[must_use]
    pub fn accessibility(&self, now: Instant) -> Option<AccessNode> {
        if !self.is_up(now) {
            return None;
        }
        let said = self
            .labels
            .iter()
            .map(|label| format!("{} {}", label.number, label.name))
            .collect::<Vec<_>>()
            .join(", ");
        let bounds = self
            .cards
            .iter()
            .fold(Rect::ZERO, |whole, card| whole.union(card.rect));
        Some(
            AccessNode::new(Role::StaticText)
                .with_hashed_key("chrome-display-identify")
                .with_live(Politeness::Polite)
                .with_value(said)
                .with_bounds(bounds),
        )
    }

    /// Paint each display's number and name, if the cards are still up. `displays` are the displays
    /// the desk shows, each with its rectangle in the desk's coordinates.
    pub fn draw(
        &mut self,
        displays: &[(String, Rect)],
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.draw_at(displays, scene, transform, text, theme, ui::time::now());
    }

    /// [`Self::draw`] at `now`, the frame's instant: a test tells the cards they are on their way
    /// with its own clock rather than whatever the machine's says when it happens to draw.
    pub fn draw_at(
        &mut self,
        displays: &[(String, Rect)],
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) {
        let motion = theme.motion();
        let Some(gone) = self.gone(now).filter(|_| !self.labels.is_empty()) else {
            self.fade_from = None;
            self.cards.clear();
            return;
        };
        let Some((_, entered)) = self.up else {
            self.cards.clear();
            return;
        };
        let fade_from = Transient::exit_starts(Transient::CONFIRMATION, gone, motion);
        self.fade_from = Some(fade_from);
        let alpha = Transient::opacity(Transient::CONFIRMATION, (entered, gone), now, motion);
        if alpha <= 0.0 {
            self.cards.clear();
            return;
        }
        let faded = alpha < 0.999;
        let mut cards = Vec::with_capacity(self.labels.len());
        for label in &self.labels {
            let Some((_, display)) = displays
                .iter()
                .find(|(monitor, _)| *monitor == label.monitor)
            else {
                continue;
            };
            let card = card_rect(*display, label.number, &label.name, text, theme);
            paint_card(
                &card,
                label,
                scene,
                transform,
                text,
                theme,
                faded.then_some(alpha),
            );
            cards.push(card);
        }
        self.cards = cards;
    }
}

/// Where one display's card stands: its bottom-left corner, inset from the edges, its number and
/// name inside the theme's padding.
fn card_rect(
    display: Rect,
    number: usize,
    name: &str,
    text: &mut TextRenderer,
    theme: &Theme,
) -> Card {
    let number_text = number.to_string();
    let number_style = theme.text(TextRole::Subheading);
    let name_style = theme.text(TextRole::Body);
    let (number_width, number_height) = text.measure_style(&number_text, None, &number_style);
    let (name_width, name_height) = text.measure_style(name, None, &name_style);
    let pad_x = f64::from(theme.tokens.spacing.s4);
    let pad_y = f64::from(theme.tokens.spacing.s2);
    let gap = f64::from(theme.tokens.spacing.s2);
    let inset = f64::from(theme.tokens.spacing.s6);
    let wanted = f64::from(number_width) + gap + f64::from(name_width) + 2.0 * pad_x;
    let width = wanted.min(display.width() / 2.0);
    let height = f64::from(number_height.max(name_height)) + 2.0 * pad_y;
    let x0 = display.x0 + inset;
    let y0 = display.y1 - inset - height;
    Card {
        display,
        rect: Rect::new(x0, y0, x0 + width, y0 + height),
        number,
        name: name.to_owned(),
    }
}

/// Paint one card: the panel, then the number in the accent and the name beside it.
fn paint_card(
    card: &Card,
    label: &Label,
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    faded: Option<f32>,
) {
    if let Some(alpha) = faded {
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            alpha,
            transform,
            &ui::widget::floating_surface::painted_bounds(card.rect, theme),
        );
    }
    ui::widget::floating_surface::paint_card(scene, transform, card.rect, theme);
    let colors = &theme.tokens.colors;
    let number_ink = colors.intent.accent_ink;
    let name_ink = colors
        .on_surface(ui::widget::floating_surface::card_ground(theme))
        .ink;
    let pad_x = f64::from(theme.tokens.spacing.s4);
    let pad_y = f64::from(theme.tokens.spacing.s2);
    let gap = f64::from(theme.tokens.spacing.s2);
    let number_style = theme.text(TextRole::Subheading);
    let name_style = theme.text(TextRole::Body);
    let number = label.number.to_string();
    let (number_width, _) = text.measure_style(&number, None, &number_style);
    let origin = (card.rect.x0 + pad_x, card.rect.y0 + pad_y);
    text.draw_style(
        scene,
        &number,
        number_ink,
        transform.pre_translate(origin.into()),
        None,
        &number_style,
    );
    let name_x = origin.0 + f64::from(number_width) + gap;
    text.draw_style(
        scene,
        &label.name,
        name_ink,
        transform.pre_translate((name_x, origin.1).into()),
        Some(name_width(card.rect, name_x, pad_x)),
        &name_style,
    );
    if faded.is_some() {
        scene.pop_layer();
    }
}

/// The width a card's name wraps at, from where it starts to the card's padding.
fn name_width(card: Rect, name_x: f64, pad_x: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a card's text width is a few hundred logical pixels"
    )]
    {
        (card.x1 - pad_x - name_x).max(0.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    const RIGHT: Rect = Rect::new(1280.0, 0.0, 2560.0, 800.0);

    fn ms(t: Instant, n: u64) -> Instant {
        t.checked_add(Duration::from_millis(n))
            .expect("test instants stay in range")
    }

    fn asked(id: u64) -> Ask {
        Ask {
            id,
            labels: vec![
                Label {
                    monitor: "eDP-1".to_owned(),
                    number: 1,
                    name: "Built-in display".to_owned(),
                },
                Label {
                    monitor: "DP-2".to_owned(),
                    number: 2,
                    name: "Dell U2724DE".to_owned(),
                },
            ],
        }
    }

    fn displays() -> Vec<(String, Rect)> {
        vec![("eDP-1".to_owned(), LEFT), ("DP-2".to_owned(), RIGHT)]
    }

    /// Paint the cards as one frame does, at the instant the test asks with, and hand back what
    /// stood on screen: the frame's own clock is the machine's, which under load can pass the
    /// cards' three seconds between the ask and the draw.
    fn painted(identify: &mut DisplayIdentify, now: Instant) -> Vec<Card> {
        let theme = Theme::dark();
        let mut text = TextRenderer::default();
        let mut scene = Scene::new();
        identify.draw_at(
            &displays(),
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            now,
        );
        identify.cards().to_vec()
    }

    #[test]
    fn every_display_says_its_number_and_name_then_goes_on_its_own() {
        let t0 = Instant::now();
        let mut identify = DisplayIdentify::default();
        assert!(!identify.is_up(t0), "nothing is said before Identify");

        identify.follow(Some(asked(7)), t0);
        assert!(identify.is_up(t0), "Identify says it at once");
        assert_eq!(
            identify.next_wake_deadline(t0),
            Some(ms(t0, 3_000)),
            "a frame is owed when the cards go"
        );

        let cards = painted(&mut identify, t0);
        assert_eq!(cards.len(), 2, "one card per display: {cards:?}");
        let builtin = &cards[0];
        assert_eq!(
            (builtin.number, builtin.name.as_str()),
            (1, "Built-in display")
        );
        assert!(
            LEFT.contains(builtin.rect.center()),
            "the card stands on its own display: {:?}",
            builtin.rect
        );
        assert!(
            builtin.rect.y0 > LEFT.center().y && builtin.rect.x0 > LEFT.x0,
            "in a corner, clear of what the person arranges: {:?}",
            builtin.rect
        );
        let external = &cards[1];
        assert_eq!(
            (external.number, external.name.as_str()),
            (2, "Dell U2724DE")
        );
        assert!(
            RIGHT.contains(external.rect.center()) && external.rect.x0 >= RIGHT.x0,
            "the second display says its own number: {:?}",
            external.rect
        );

        assert!(
            identify.is_up(ms(t0, 2_900)),
            "the cards stay up long enough to look from the canvas"
        );
        assert!(
            !identify.is_up(ms(t0, 3_000)),
            "and are gone after three seconds"
        );
    }

    #[test]
    fn asking_again_starts_the_cards_over() {
        let t0 = Instant::now();
        let mut identify = DisplayIdentify::default();
        identify.follow(Some(asked(7)), t0);
        identify.follow(Some(asked(7)), ms(t0, 1_000));
        assert_eq!(
            identify.next_wake_deadline(ms(t0, 1_000)),
            Some(ms(t0, 3_000)),
            "the same request answers once: its cards do not start over"
        );
        identify.follow(Some(asked(8)), ms(t0, 2_000));
        assert_eq!(
            identify.next_wake_deadline(ms(t0, 2_000)),
            Some(ms(t0, 5_000)),
            "a second press of Identify shows the cards again"
        );
    }

    #[test]
    fn a_display_the_desk_does_not_show_gets_no_card() {
        let t0 = Instant::now();
        let mut identify = DisplayIdentify::default();
        let ask = Ask {
            id: 3,
            labels: vec![
                Label {
                    monitor: "eDP-1".to_owned(),
                    number: 1,
                    name: "Built-in display".to_owned(),
                },
                Label {
                    monitor: "HDMI-A-1".to_owned(),
                    number: 2,
                    name: "External display".to_owned(),
                },
            ],
        };
        identify.follow(Some(ask), t0);
        let cards = painted(&mut identify, t0);
        assert_eq!(cards.len(), 1, "only the display that is there: {cards:?}");
        assert_eq!(cards[0].name, "Built-in display");
    }

    #[test]
    fn what_the_cards_say_is_read_once_and_takes_no_pointer() {
        let t0 = Instant::now();
        let mut identify = DisplayIdentify::default();
        identify.follow(Some(asked(7)), t0);
        painted(&mut identify, t0);
        let node = identify
            .accessibility(t0)
            .expect("the cards are announced while they stand");
        assert_eq!(
            node.value.as_deref(),
            Some("1 Built-in display, 2 Dell U2724DE")
        );
        assert!(identify.accessibility(ms(t0, 3_000)).is_none());
    }
}
