//! Identify displays: a large card with the display's number and name on
//! every attached display for a few seconds, answering "which screen is
//! which?" while arranging them in Settings. A request naming one display
//! (the one the person points at or picks in Settings) flashes that
//! display's card alone, briefly.
//!
//! Settings asks through [`IdentifyRequest`]; the numbers come from
//! [`display_numbers`] for a local setup, or from the current shared display
//! order when a native monitor matches its observed stable identity, and the
//! name from [`display_names`](settings_store::display_identify::display_names), so the card says what Settings' canvas tile
//! and display picker say. The number is in the accent, as the tile's is
//! while the cards show. Cards arrive and fade as a transient readout
//! (`motion::Transient`); between the fades they ask for no frames.

use std::cell::Cell;
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
use ui::text_render::{TextRenderer, TextWeight};
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
        self.start_only(
            outputs,
            DisplayHardwareSnapshot::load().as_ref(),
            request.display.as_deref(),
            Duration::from_millis(request.duration_ms()),
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
        self.start_only(outputs, snapshot, None, duration, now);
    }

    /// [`Self::start`], on the output whose key is `only` when it names one.
    pub(crate) fn start_only(
        &mut self,
        outputs: &[OutputGeometry],
        snapshot: Option<&DisplayHardwareSnapshot>,
        only: Option<&str>,
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
            .filter_map(|output| {
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
                if let Some(only) = only
                    && placed
                        .as_ref()
                        .is_none_or(|(described, _, _)| described.descriptor.connector != only)
                {
                    return None;
                }
                Some(if let Some((_, number, name)) = placed {
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
                })
            })
            .collect();
        // A flash on a display whose card already shows keeps the card up
        // rather than fading it in again.
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

    pub(crate) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) {
        self.motion.set(theme.motion());
        let labels = self.labels(now);
        let opacity = self.opacity(now);
        self.painted.set(!labels.is_empty() && opacity > 0.0);
        if !self.painted.get() {
            return;
        }
        let type_scale = &theme.tokens.typography;
        for label in labels {
            let side = (label.output.width().min(label.output.height()) * 0.36).clamp(160.0, 420.0);
            let card = Rect::from_center_size(label.output.center(), (side * 1.4, side));
            let faded = opacity < 0.999;
            if faded {
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    opacity,
                    transform,
                    &card.inflate(
                        f64::from(theme.tokens.spacing.s8),
                        f64::from(theme.tokens.spacing.s8),
                    ),
                );
            }
            let painted = ui::chrome::shell::paint_panel(scene, transform, card, theme, 1.0);
            let theme = &painted.theme;
            let colors = &theme.tokens.colors;
            let number = label.number.to_string();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a display-sized glyph fits f32"
            )]
            let number_size = (side * 0.52) as f32;
            let (nw, nh) =
                text_renderer.measure_weighted(&number, number_size, None, TextWeight::Semibold);
            text_renderer.draw_text_weighted(
                scene,
                &number,
                number_size,
                colors.intent.accent_ink,
                transform.pre_translate(
                    (
                        card.center().x - f64::from(nw) / 2.0,
                        card.center().y - f64::from(nh) / 2.0 - side * 0.1,
                    )
                        .into(),
                ),
                None,
                TextWeight::Semibold,
            );
            #[allow(clippy::cast_possible_truncation, reason = "card width fits f32")]
            let name_width = (card.width() - f64::from(theme.tokens.spacing.s8)) as f32;
            let name_style = type_scale.style(TextRole::Heading);
            let name = text_renderer.elide_to_width_style(&label.name, name_width, &name_style);
            let (lw, _) = text_renderer.measure_style(&name, None, &name_style);
            text_renderer.draw_style(
                scene,
                &name,
                colors.text_primary,
                transform.pre_translate(
                    (card.center().x - f64::from(lw) / 2.0, card.y1 - side * 0.24).into(),
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

    #[test]
    fn a_flash_names_one_display_and_shows_on_it_alone() {
        let (snapshot, outputs) = two_displays();
        let mut identify = DisplayIdentify::default();
        let now = Instant::now();
        identify.start_only(
            &outputs,
            Some(&snapshot),
            Some("DP-1"),
            Duration::from_millis(settings_store::display_identify::IDENTIFY_FLASH_MS),
            now,
        );
        let labels = identify.labels(now);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].output, outputs[1].rect());
        assert_eq!(labels[0].number, 2);
    }
}
