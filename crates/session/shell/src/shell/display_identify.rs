//! Identify displays: a large number and the display's name painted on
//! every attached display for a few seconds, answering "which screen is
//! which?" while arranging them in Settings.
//!
//! Settings asks through [`IdentifyRequest`]; the numbers come from
//! [`display_numbers`] for a local setup, or from the current shared display
//! order when a native monitor matches its observed stable identity.

use std::time::{Duration, Instant};

use settings_store::display_identify::{IDENTIFY_MS, IdentifyRequest, display_numbers};
use settings_store::hardware::DisplayHardwareSnapshot;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::i18n::ArgValue;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::shell::i18n::{t, t_args};
use crate::shell::overlay_host::OutputGeometry;
use ui::TextRole;

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
    until: Option<Instant>,
    seen: Option<u64>,
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
            Duration::from_millis(IDENTIFY_MS),
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
        let shown = outputs
            .iter()
            .filter(|output| output.width > 0.0 && output.height > 0.0);
        for (label, output) in self.labels.iter_mut().zip(shown) {
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
                    Some((described, numbers.get(index).copied()?))
                });
                if let Some((described, number)) = placed {
                    IdentifyLabel {
                        output: output.rect(),
                        number,
                        name: if described.builtin {
                            t("display-identify-builtin")
                        } else {
                            described.descriptor.label().to_owned()
                        },
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

    /// Frames are needed while cards show, and briefly after, so the frame
    /// that clears them is painted.
    pub(crate) fn needs_frame(&self, now: Instant) -> bool {
        self.until
            .and_then(|until| until.checked_add(Duration::from_millis(100)))
            .is_some_and(|end| now < end)
    }

    pub(crate) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        for label in self.labels(now) {
            let side = (label.output.height() * 0.28).clamp(120.0, 260.0);
            let card = Rect::from_center_size(label.output.center(), (side * 1.25, side));
            ui::chrome::shell::paint_panel(scene, transform, card, theme, 1.0);
            let number = label.number.to_string();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a display-sized glyph fits f32"
            )]
            let number_size = (side * 0.5) as f32;
            let (nw, nh) =
                text_renderer.measure_weighted(&number, number_size, None, TextWeight::Semibold);
            text_renderer.draw_text_weighted(
                scene,
                &number,
                number_size,
                colors.text_primary,
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
            let name_width = (card.width() - 32.0) as f32;
            let name = text_renderer.elide_to_width_style(
                &label.name,
                name_width,
                &type_scale.style(TextRole::Heading),
            );
            let (lw, _) =
                text_renderer.measure_style(&name, None, &type_scale.style(TextRole::Heading));
            text_renderer.draw_style(
                scene,
                &name,
                colors.text_dim,
                transform.pre_translate(
                    (card.center().x - f64::from(lw) / 2.0, card.y1 - side * 0.22).into(),
                ),
                None,
                &type_scale.style(TextRole::Heading),
            );
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
        // The monitor sits above the laptop panel, sharing its left edge.
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
        assert_eq!(labels[0].name, "DP-1");
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
}
