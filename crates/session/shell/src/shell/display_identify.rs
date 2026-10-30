//! Identify displays: a small card with the display's number and short
//! name in a corner of every attached display for a few seconds, answering
//! "which screen is which?" while arranging them in Settings. The card keeps
//! out of the way of that work: it never covers a display's centre, and of
//! the four corners it takes the one the focused window (Settings, with its
//! arrangement canvas) covers least ([`card_rect`]).
//!
//! Settings asks through [`IdentifyRequest`], only when the person presses
//! Identify; the numbers come from [`display_numbers`] for a local setup, or
//! from the current shared display order when a native monitor matches its
//! observed stable identity, and the name from
//! [`display_names`](settings_store::display_identify::display_names), so the
//! card says what Settings' canvas tile says. The number is in the accent, as
//! the tile's is while the cards show. Cards arrive and fade as a transient
//! readout (`motion::Transient`); between the fades they ask for no frames.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use settings_store::display_identify::{
    IdentifyRequest, display_name, display_numbers, name_apart,
};
use settings_store::hardware::DisplayHardwareSnapshot;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Fill};

use ui::i18n::ArgValue;
use ui::motion::Transient;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::EffectiveMotion;

use crate::shell::i18n::{t, t_args};
use crate::shell::overlay_host::OutputGeometry;
use ui::TextRole;

/// How identify cards come and go.
const MOTION: ui::motion::MotionSpec = Transient::READOUT;

/// One display's identify card.
#[derive(Clone, Debug, PartialEq)]
pub struct IdentifyLabel {
    /// The whole display, in combined desktop coordinates.
    pub output: Rect,
    pub number: usize,
    pub name: String,
}

#[derive(Default)]
pub(crate) struct DisplayIdentify {
    labels: Vec<IdentifyLabel>,
    shown: Option<Instant>,
    until: Option<Instant>,
    seen: Option<u64>,
    /// The motion the last paint read the theme at.
    motion: Cell<EffectiveMotion>,
    /// Whether the last paint put a card on screen, so the frame that
    /// clears the last one is painted.
    painted: Cell<bool>,
    /// Where the last paint put each card, in desktop coordinates.
    cards: RefCell<Vec<Rect>>,
}

/// What identify cards keep clear of.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct KeepClear {
    /// The band along each display's top the top bar takes.
    pub top_band: f64,
    /// The focused window: Settings, with the arrangement canvas.
    pub window: Option<Rect>,
}

/// Where a card of `size` sits on `output`: `inset` in from one of its corners,
/// the corner whose card `avoid` (the focused window) covers least, the
/// bottom-left first among equals. Top corners sit below the `top_band`
/// the top bar takes, so a card never covers it.
#[must_use]
pub fn card_rect(
    output: Rect,
    size: (f64, f64),
    inset: f64,
    top_band: f64,
    avoid: Option<Rect>,
) -> Rect {
    let (width, height) = (
        size.0.min(output.width() / 2.0 - inset).max(0.0),
        size.1.min(output.height() / 2.0 - inset).max(0.0),
    );
    let left = output.x0 + inset;
    let right = output.x1 - inset - width;
    let top = output.y0 + top_band.max(0.0) + inset;
    let bottom = output.y1 - inset - height;
    let corners = [(left, bottom), (right, bottom), (right, top), (left, top)]
        .map(|(x, y)| Rect::new(x, y, x + width, y + height));
    let covered = |card: &Rect| avoid.map_or(0.0, |avoid| card.intersect(avoid).area());
    corners
        .iter()
        .copied()
        .enumerate()
        .min_by(|(a_index, a), (b_index, b)| {
            covered(a).total_cmp(&covered(b)).then(a_index.cmp(b_index))
        })
        .map_or(corners[0], |(_, card)| card)
}

impl DisplayIdentify {
    /// Start showing numbers when Settings has asked since the last look.
    /// Returns whether the overlay started.
    pub(crate) fn poll(&mut self, outputs: &[OutputGeometry], now: Instant) -> bool {
        let Some(request) = IdentifyRequest::load() else {
            return false;
        };
        if self.seen == Some(request.requested_unix_ms) {
            return false;
        }
        self.seen = Some(request.requested_unix_ms);
        if !request.is_fresh(tilo_clock::unix_now_ms()) {
            return false;
        }
        self.start(
            outputs,
            DisplayHardwareSnapshot::load().as_ref(),
            Duration::from_millis(settings_store::display_identify::IDENTIFY_MS),
            now,
        );
        true
    }

    /// Number this device's displays by their place in the person's shared
    /// display order, which spans all of their devices.
    pub(crate) fn number_by_shared_order(
        &mut self,
        outputs: &[OutputGeometry],
        layout: &settings_store::person_displays::Layout,
    ) {
        for label in &mut self.labels {
            let Some(output) = outputs.iter().find(|output| {
                output.width > 0.0 && output.height > 0.0 && output.rect() == label.output
            }) else {
                continue;
            };
            if let Some(shared) = layout.displays.iter().find(|display| {
                !output.monitor.is_empty() && display.local && display.output == output.monitor
            }) {
                label.number = shared.order.saturating_add(1);
            }
        }
    }

    /// Label every output for `duration`. Outputs the compositor's snapshot
    /// describes get their number and name; any other output still gets a
    /// card, numbered after them, so no screen is left unexplained.
    pub(crate) fn start(
        &mut self,
        outputs: &[OutputGeometry],
        snapshot: Option<&DisplayHardwareSnapshot>,
        duration: Duration,
        now: Instant,
    ) {
        let numbers = snapshot
            .map(|s| display_numbers(&s.outputs))
            .unwrap_or_default();
        // The names Settings shows, the built-in panel's in this language.
        let names = snapshot
            .map(|s| {
                let names = s
                    .outputs
                    .iter()
                    .map(|output| {
                        if output.builtin {
                            t("display-identify-builtin")
                        } else {
                            display_name(&output.descriptor, false)
                        }
                    })
                    .collect::<Vec<_>>();
                name_apart(&names, &numbers)
            })
            .unwrap_or_default();
        let known = numbers.len();
        let mut next_unknown = known;
        self.labels = outputs
            .iter()
            .filter(|output| output.width > 0.0 && output.height > 0.0)
            .map(|output| {
                let placed = snapshot.and_then(|snapshot| {
                    // Matched by origin: a mirror shares its target's
                    // picture, so it shows the target's card anyway.
                    let placement = snapshot.layout.iter().find(|placement| {
                        (f64::from(placement.x) - output.x).abs() < 0.5
                            && (f64::from(placement.y) - output.y).abs() < 0.5
                    })?;
                    let index = snapshot
                        .outputs
                        .iter()
                        .position(|o| o.descriptor.connector == placement.connector)?;
                    let described = snapshot.outputs.get(index)?;
                    Some((
                        described,
                        numbers.get(index).copied()?,
                        names.get(index).cloned()?,
                    ))
                });
                if let Some((_, number, name)) = placed {
                    IdentifyLabel {
                        output: output.rect(),
                        number,
                        name,
                    }
                } else {
                    next_unknown = next_unknown.saturating_add(1);
                    IdentifyLabel {
                        output: output.rect(),
                        number: next_unknown,
                        name: t_args(
                            "display-identify-unnamed",
                            &[("number", ArgValue::Text(next_unknown.to_string()))],
                        ),
                    }
                }
            })
            .collect();
        // Identify pressed again while the cards show keeps them up rather
        // than fading them in again.
        let showing = self.shown.is_some() && self.until.is_some_and(|until| now < until);
        if !showing {
            self.shown = Some(now);
        }
        self.until = now.checked_add(duration);
    }

    /// The cards on screen at `now`.
    pub(crate) fn labels(&self, now: Instant) -> &[IdentifyLabel] {
        if self.until.is_some_and(|until| now < until) {
            &self.labels
        } else {
            &[]
        }
    }

    /// The cards' opacity at `now`: arriving, held, fading out by the end.
    pub(crate) fn opacity(&self, now: Instant) -> f32 {
        match (self.shown, self.until) {
            (Some(shown), Some(until)) => {
                Transient::opacity(MOTION, (shown, until), now, self.motion.get())
            }
            _ => 0.0,
        }
    }

    /// Frames are needed while the cards arrive or fade, and once after
    /// they went, so the frame that clears them is painted. A held card
    /// asks for none.
    pub(crate) fn needs_frame(&self, now: Instant) -> bool {
        let (Some(shown), Some(until)) = (self.shown, self.until) else {
            return false;
        };
        Transient::is_moving(MOTION, (shown, until), now, self.motion.get())
            || (now >= until && self.painted.get())
    }

    /// When the cards next change while held: the fade's start, then their
    /// end (overdue until the clearing frame is painted).
    pub(crate) fn next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        let until = self.until?;
        let fade = Transient::exit_starts(MOTION, until, self.motion.get());
        if now < fade {
            Some(fade)
        } else if now < until || self.painted.get() {
            Some(until)
        } else {
            None
        }
    }

    /// Where the last paint put each card, in desktop coordinates.
    pub(crate) fn cards(&self) -> Vec<Rect> {
        self.cards.borrow().clone()
    }

    /// Whether a card shows at `now`.
    pub(crate) fn showing(&self, now: Instant) -> bool {
        !self.labels(now).is_empty()
    }

    /// Paint each display's card in a corner, clear of what `clear` names.
    pub(crate) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
        clear: KeepClear,
    ) {
        self.motion.set(theme.motion());
        let labels = self.labels(now);
        let opacity = self.opacity(now);
        self.painted.set(!labels.is_empty() && opacity > 0.0);
        let mut cards = self.cards.borrow_mut();
        cards.clear();
        if !self.painted.get() {
            return;
        }
        let spacing = &theme.tokens.spacing;
        let (pad_x, pad_y, gap) = (
            f64::from(spacing.s3),
            f64::from(spacing.s2),
            f64::from(spacing.s2),
        );
        let number_style = theme.text(TextRole::Title).strong().numeric();
        let name_style = theme.text(TextRole::Label);
        for label in labels {
            let number = label.number.to_string();
            let (nw, nh) = text_renderer.measure_style(&number, None, &number_style);
            // A name runs no wider than the narrowest panel before it is
            // elided.
            let name = text_renderer.elide_to_width_style(
                &label.name,
                theme.tokens.metrics.panel_min_width,
                &name_style,
            );
            let (lw, lh) = text_renderer.measure_style(&name, None, &name_style);
            let size = (
                pad_x + f64::from(nw) + gap + f64::from(lw) + pad_x,
                pad_y + f64::from(nh.max(lh)) + pad_y,
            );
            let card = card_rect(
                label.output,
                size,
                f64::from(spacing.s6),
                clear.top_band,
                clear.window,
            );
            cards.push(card);
            let faded = opacity < 0.999;
            if faded {
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    opacity,
                    transform,
                    &card.inflate(f64::from(spacing.s6), f64::from(spacing.s6)),
                );
            }
            let painted = ui::chrome::shell::paint_panel(scene, transform, card, theme, 1.0);
            let colors = &painted.theme.tokens.colors;
            let middle = card.center().y;
            text_renderer.draw_style(
                scene,
                &number,
                colors.intent.accent_ink,
                transform.pre_translate((card.x0 + pad_x, middle - f64::from(nh) / 2.0).into()),
                None,
                &number_style,
            );
            text_renderer.draw_style(
                scene,
                &name,
                colors.text_primary,
                transform.pre_translate(
                    (
                        card.x0 + pad_x + f64::from(nw) + gap,
                        middle - f64::from(lh) / 2.0,
                    )
                        .into(),
                ),
                None,
                &name_style,
            );
            if faded {
                scene.pop_layer();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Point;

    /// The inset the identify cards sit at on the base theme.
    const CARD_INSET: f64 = ui::tokens::Spacing::BASE.s6 as f64;

    #[test]
    fn every_output_gets_a_card_even_without_a_snapshot() {
        let outputs = [
            OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0),
            OutputGeometry::unscaled(1920.0, 0.0, 2560.0, 1440.0),
        ];
        let mut identify = DisplayIdentify::default();
        let now = Instant::now();
        identify.start(&outputs, None, Duration::from_secs(3), now);
        let labels = identify.labels(now);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].number, 1);
        assert_eq!(labels[1].output, Rect::new(1920.0, 0.0, 4480.0, 1440.0));
        assert!(identify.labels(now + Duration::from_secs(3)).is_empty());
    }

    #[test]
    fn stacked_displays_are_told_apart_by_their_origin() {
        use settings_store::hardware::{
            DisplayOutputSnapshot, LiveOutputPlacement, OutputDescriptor, OutputMode,
        };
        let mode = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        let described = |connector: &str, builtin: bool| DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.into(),
                connector_label: connector.into(),
                modes: vec![mode],
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: None,
            },
            current_mode: mode,
            scale: 1.0,
            builtin,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: std::collections::BTreeMap::new(),
        };
        let placed = |connector: &str, y: i32| LiveOutputPlacement {
            connector: connector.into(),
            x: 0,
            y,
            width: 1920,
            height: 1080,
        };
        let snapshot = DisplayHardwareSnapshot {
            outputs: vec![described("eDP-1", true), described("DP-1", false)],
            layout: vec![placed("DP-1", 0), placed("eDP-1", 1080)],
            ..DisplayHardwareSnapshot::default()
        };
        let outputs = [
            OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0),
            OutputGeometry::unscaled(0.0, 1080.0, 1920.0, 1080.0),
        ];
        let mut identify = DisplayIdentify::default();
        let now = Instant::now();
        identify.start(&outputs, Some(&snapshot), Duration::from_secs(3), now);
        let labels = identify.labels(now);
        assert_eq!(labels[0].name, "External display", "never its port");
        assert_eq!(labels[0].number, 2);
        assert_eq!(labels[1].number, 1, "the built-in panel below is display 1");
        assert_eq!(labels[1].output, Rect::new(0.0, 1080.0, 1920.0, 2160.0));
    }

    #[test]
    fn numbers_follow_the_persons_shared_display_order() {
        use settings_store::person_displays::{
            ArrangedDisplay, Arrangement, DeviceOutput, Layout, PersonDevice,
        };
        let output = |id: &str, x: i32| DeviceOutput {
            id: id.into(),
            name: id.into(),
            builtin: false,
            x,
            y: 0,
            width: 1920,
            height: 1080,
            mode_width: 1920,
            mode_height: 1080,
            refresh_mhz: 60_000,
            scale_milli: 1000,
            connected: true,
            enabled: true,
        };
        let device = |id: &str, local, outputs| PersonDevice {
            device: id.into(),
            label: id.into(),
            local,
            online: true,
            outputs,
        };
        // The Pi's screen comes first in the person's order.
        let layout = Layout::resolve(
            &[
                device(
                    "laptop",
                    true,
                    vec![output("left", 0), output("right", 1920)],
                ),
                device("pi", false, vec![output("hdmi", 0)]),
            ],
            &Arrangement {
                displays: vec![ArrangedDisplay {
                    device: "pi".into(),
                    output: "hdmi".into(),
                    enabled: true,
                    order: 0,
                    placed: true,
                    x: -1920,
                    y: 0,
                    rev: 1,
                }],
            },
        );
        let mut outputs = [
            OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0),
            OutputGeometry::unscaled(1920.0, 0.0, 1920.0, 1080.0),
        ];
        outputs[0].monitor = "left".into();
        outputs[1].monitor = "right".into();
        let mut identify = DisplayIdentify::default();
        let now = Instant::now();
        identify.start(&outputs, None, Duration::from_secs(3), now);
        identify.number_by_shared_order(&outputs, &layout);
        let numbers: Vec<_> = identify
            .labels(now)
            .iter()
            .map(|label| label.number)
            .collect();
        assert_eq!(numbers, [2, 3]);
        assert_eq!(identify.labels(now)[0].output, outputs[0].rect());
        outputs[0].monitor = "unobserved-monitor".into();
        identify.start(&outputs, None, Duration::from_secs(3), now);
        identify.number_by_shared_order(&outputs, &layout);
        assert_eq!(
            identify.labels(now)[0].number,
            1,
            "geometry cannot substitute for a stable monitor identity"
        );
        assert_eq!(identify.labels(now)[1].number, 3);
    }

    fn two_displays() -> (DisplayHardwareSnapshot, [OutputGeometry; 2]) {
        use settings_store::hardware::{
            DisplayOutputSnapshot, LiveOutputPlacement, MonitorIdentity, OutputDescriptor,
            OutputMode,
        };
        let mode = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        let described = |connector: &str, builtin: bool, monitor| DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.into(),
                connector_label: connector.into(),
                modes: vec![mode],
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor,
            },
            current_mode: mode,
            scale: 1.0,
            builtin,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: std::collections::BTreeMap::new(),
        };
        let placed = |connector: &str, x: i32| LiveOutputPlacement {
            connector: connector.into(),
            x,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let snapshot = DisplayHardwareSnapshot {
            outputs: vec![
                described("eDP-1", true, None),
                described(
                    "DP-1",
                    false,
                    Some(MonitorIdentity {
                        make: "Dell".into(),
                        model: "DELL U2720Q".into(),
                        ..MonitorIdentity::default()
                    }),
                ),
            ],
            layout: vec![placed("eDP-1", 0), placed("DP-1", 1920)],
            ..DisplayHardwareSnapshot::default()
        };
        let outputs = [
            OutputGeometry::unscaled(0.0, 0.0, 1920.0, 1080.0),
            OutputGeometry::unscaled(1920.0, 0.0, 1920.0, 1080.0),
        ];
        (snapshot, outputs)
    }

    #[test]
    fn identify_shows_on_every_output_with_the_numbers_and_names_settings_shows() {
        let (snapshot, outputs) = two_displays();
        let numbers = display_numbers(&snapshot.outputs);
        let mut identify = DisplayIdentify::default();
        let now = Instant::now();
        let duration = Duration::from_millis(settings_store::display_identify::IDENTIFY_MS);
        identify.start(&outputs, Some(&snapshot), duration, now);
        let labels = identify.labels(now).to_vec();
        assert_eq!(labels.len(), 2, "a card on every display");
        assert_eq!(labels[0].output, outputs[0].rect());
        assert_eq!(labels[1].output, outputs[1].rect());
        assert_eq!(
            labels.iter().map(|label| label.number).collect::<Vec<_>>(),
            numbers,
            "the numbers Settings numbers the canvas with"
        );
        assert_eq!(
            labels[1].name,
            settings_store::display_identify::display_names(&snapshot.outputs)[1],
            "the name Settings shows"
        );
        assert_eq!(labels[1].name, "Dell U2720Q", "by model, never by port");
        // It fades in, holds without asking for frames, and fades out by
        // the time it goes.
        assert!(identify.opacity(now) < 1.0, "it arrives, not pops");
        assert!(identify.needs_frame(now));
        let held = now + duration / 2;
        assert_eq!(identify.opacity(held), 1.0);
        assert!(!identify.needs_frame(held), "a held card costs no frames");
        assert!(
            identify
                .next_wake_deadline(held)
                .is_some_and(|at| at > held)
        );
        let fading = (now + duration)
            .checked_sub(Duration::from_millis(40))
            .unwrap_or(now);
        assert!(identify.opacity(fading) < 1.0, "it fades out");
        assert!(identify.labels(now + duration).is_empty());
    }

    /// The card is small, sits in a corner and keeps off the centre.
    fn assert_out_of_the_way(card: Rect, output: Rect) {
        assert!(
            card.width() <= 320.0 && card.height() <= 64.0,
            "a compact card: {card:?}"
        );
        assert!(
            card.area() < output.area() * 0.02,
            "a sliver of the display: {card:?} on {output:?}"
        );
        assert!(
            output.contains(card.origin()) && output.contains(Point::new(card.x1, card.y1)),
            "on its own display: {card:?} on {output:?}"
        );
        assert!(!card.contains(output.center()), "never over the centre");
        let near = |a: f64, b: f64| (a - b).abs() <= CARD_INSET + 0.5;
        assert!(
            (near(card.x0, output.x0) || near(card.x1, output.x1))
                && (near(card.y0, output.y0) || near(card.y1, output.y1)),
            "in a corner: {card:?} on {output:?}"
        );
    }

    #[test]
    fn the_card_is_small_and_in_a_corner_clear_of_the_centre() {
        let size = (180.0, 44.0);
        for output in [
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            Rect::new(1920.0, -360.0, 4480.0, 1080.0),
            Rect::new(0.0, 0.0, 1280.0, 800.0),
        ] {
            let card = card_rect(output, size, CARD_INSET, 0.0, None);
            assert_out_of_the_way(card, output);
            assert_eq!(
                (card.x0, card.y1),
                (output.x0 + CARD_INSET, output.y1 - CARD_INSET),
                "bottom-left when nothing is in the way"
            );
        }
    }

    #[test]
    fn the_card_takes_the_corner_the_focused_window_leaves_free() {
        let output = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let size = (180.0, 44.0);
        // Settings on the left half, its arrangement canvas inside it.
        let settings = Rect::new(8.0, 40.0, 1100.0, 1072.0);
        let card = card_rect(output, size, CARD_INSET, 0.0, Some(settings));
        assert_out_of_the_way(card, output);
        assert!(!card.overlaps(settings), "{card:?} covers Settings");
        assert!(card.x0 > settings.x1, "the right-hand corner: {card:?}");
        // Settings across the bottom: a top corner, right first (the top
        // bar's start is on the left).
        let settings = Rect::new(0.0, 300.0, 1920.0, 1080.0);
        let card = card_rect(output, size, CARD_INSET, 0.0, Some(settings));
        assert_out_of_the_way(card, output);
        assert!(!card.overlaps(settings));
        assert!(card.x1 > output.center().x, "{card:?}");
        // A window over the whole display: the card still stays small and
        // in a corner, never over the middle of the window's content.
        let card = card_rect(output, size, CARD_INSET, 0.0, Some(output));
        assert_out_of_the_way(card, output);
        // A top corner sits below the top bar.
        let settings = Rect::new(0.0, 300.0, 1920.0, 1080.0);
        let card = card_rect(output, size, CARD_INSET, 56.0, Some(settings));
        assert!(card.y0 >= 56.0 + CARD_INSET, "{card:?} covers the top bar");
    }

    #[test]
    fn identify_paints_compact_corner_cards_and_says_where() {
        let (snapshot, outputs) = two_displays();
        let mut identify = DisplayIdentify::default();
        let now = Instant::now();
        let duration = Duration::from_millis(settings_store::display_identify::IDENTIFY_MS);
        identify.start(&outputs, Some(&snapshot), duration, now);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let settings = Rect::new(0.0, 0.0, 1300.0, 1080.0);
        identify.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            now + duration / 2,
            KeepClear {
                top_band: 56.0,
                window: Some(settings),
            },
        );
        let cards = identify.cards();
        assert_eq!(cards.len(), 2, "a card on every display");
        for (card, output) in cards.iter().zip(&outputs) {
            assert_out_of_the_way(*card, output.rect());
        }
        assert!(
            !cards[0].overlaps(settings),
            "{:?} covers Settings",
            cards[0]
        );
        identify.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            now + duration,
            KeepClear::default(),
        );
        assert!(identify.cards().is_empty(), "gone when Identify ends");
    }
}
