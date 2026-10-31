//! [`ProgressRing`] and [`BarGauge`] as described components.

use crate::component::{Component, Described};
use crate::widget::Tone;

use super::{
    BarGauge, Fillcolor, GaugeStyle, GaugeThresholds, ProgressRing, ProgressRingSize, RingFill,
};

crate::component::choice!(ProgressRingSize, "ProgressRingSize", {
    Small => "small",
    Regular => "regular",
    Large => "large",
});

impl GaugeThresholds {
    /// The thresholds a component gives, each the default where it gives none.
    #[expect(clippy::cast_possible_truncation, reason = "a share from 0 to 1")]
    pub(crate) fn given(warning: Option<f64>, danger: Option<f64>) -> Self {
        let default = Self::default();
        Self {
            warning: warning.map_or(default.warning, |w| w as f32),
            danger: danger.map_or(default.danger, |d| d as f32),
        }
    }
}

/// How full something is, as a ring: a countdown, a download, the one hero reading of a screen
/// (a speed test). A regular or large ring holds its readout and what it measures.
#[derive(Component, Default)]
#[component(name = "ProgressRing", role = "ProgressBar", label = "caption")]
pub struct ProgressRingProps {
    /// How full it is, from 0 to 1.
    #[component(required, positional)]
    pub value: f64,
    /// The readout inside the ring: "42%", "23".
    pub readout: Option<String>,
    /// What the readout measures, under it: "seconds", "downloaded".
    pub caption: Option<String>,
    /// How large it is: `regular` unless said, `small` inline beside text without a readout,
    /// `large` for the one hero reading of a screen.
    pub size: Option<ProgressRingSize>,
    /// A status the fill means: a failed transfer stays danger.
    pub tone: Option<Tone>,
    /// A measured level rather than progress: the fill turns warning, then danger, as it fills.
    pub level: bool,
}

impl ProgressRingProps {
    fn fill(&self) -> RingFill {
        match self.tone {
            Some(tone) => RingFill::Tone(tone),
            None if self.level => RingFill::Level(GaugeThresholds::default()),
            None => RingFill::Progress,
        }
    }
}

impl Described for ProgressRing {
    type Props = ProgressRingProps;

    fn from_props(_key: &str, props: ProgressRingProps) -> Self {
        let fill = props.fill();
        let mut ring = Self::new(props.value)
            .value(props.readout.unwrap_or_default())
            .caption(props.caption.unwrap_or_default())
            .size(props.size.unwrap_or_default());
        ring.fill = fill;
        ring
    }

    /// A new reading fills in place, as a ticking countdown does.
    fn set_props(&mut self, props: ProgressRingProps) -> bool {
        if props.size.unwrap_or_default() != self.size || props.fill() != self.fill {
            return false;
        }
        self.set(props.value, props.readout.unwrap_or_default());
        self.set_caption(props.caption.unwrap_or_default());
        true
    }
}

/// A labelled level as a bar: a disk's use, a quota, a battery. Its label stands over the
/// track's start and its readout over its end; the fill turns warning, then danger, as it
/// fills, unless the share has no status.
#[derive(Component, Default)]
#[component(name = "BarGauge", role = "ProgressBar", label = "label")]
pub struct BarGaugeProps {
    /// How full it is, from 0 to 1.
    #[component(required, positional)]
    pub value: f64,
    /// What it measures: "Storage", "Memory".
    pub label: Option<String>,
    /// The reading over the track's end: "412 GB of 512 GB". Unless said, the share as a
    /// percentage is announced.
    pub readout: Option<String>,
    /// A share with no status (storage by kind, a quota with room): the plain fill, never a
    /// status colour.
    pub neutral: bool,
    /// Where the fill turns warning, from 0 to 1: 0.6 unless said.
    pub warning: Option<f64>,
    /// Where the fill turns danger, from 0 to 1: 0.85 unless said.
    pub danger: Option<f64>,
    /// Smaller label and readout, for a dense grid of gauges.
    pub compact: bool,
}

impl Described for BarGauge {
    type Props = BarGaugeProps;

    #[expect(
        clippy::cast_possible_truncation,
        reason = "a share from 0 to 1, clamped by the gauge"
    )]
    fn from_props(_key: &str, props: BarGaugeProps) -> Self {
        let BarGaugeProps {
            value,
            label,
            readout,
            neutral,
            warning,
            danger,
            compact,
        } = props;
        let mut gauge = Self::new(value as f32)
            .with_label(label.unwrap_or_default(), readout.unwrap_or_default());
        gauge.fill = if neutral {
            Fillcolor::Neutral
        } else {
            Fillcolor::Threshold(GaugeThresholds::given(warning, danger))
        };
        if compact {
            gauge = gauge.with_style(GaugeStyle::default().compact());
        }
        gauge
    }
}
