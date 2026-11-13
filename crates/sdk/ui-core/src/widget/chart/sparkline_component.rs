//! [`Sparkline`] as a described component: a tiny inline trend line.

use crate::component::{Component, Described};
use crate::widget::Tone;

use super::Sparkline;

/// A tiny trend line, with no axis, grid, title or legend: a recent history inside a table cell or a
/// stat card.
#[derive(Component, Default)]
#[component(name = "Sparkline", role = "Image", label = "label")]
pub struct SparklineProps {
    /// The samples, oldest first.
    #[component(required, positional)]
    pub samples: Vec<f64>,
    /// What the trend is of, as assistive technology announces it: "Requests per second".
    pub label: Option<String>,
    /// The vertical maximum, which may be more than the samples reach; without one it is the highest
    /// sample.
    pub max: Option<f64>,
    /// The area under the line filled in the same colour, which a sparkline draws unless it is told not to:
    /// `true` unless said.
    pub filled: Option<bool>,
    /// The line's shape between samples: the smooth curve rather than straight segments.
    pub smooth: bool,
    /// A dot of this radius on the newest sample: the live edge of a streaming series.
    pub leading_dot: Option<f64>,
    /// What its line means, when it means something: a status, a person.
    pub tone: Option<Tone>,
}

impl Described for Sparkline {
    type Props = SparklineProps;

    #[expect(
        clippy::cast_possible_truncation,
        reason = "a trend sample, well inside f32"
    )]
    fn from_props(_key: &str, props: SparklineProps) -> Self {
        let SparklineProps {
            samples,
            label,
            max,
            filled,
            smooth,
            leading_dot,
            tone,
        } = props;
        let data: Vec<f32> = samples.iter().map(|sample| *sample as f32).collect();
        let mut spark = Self::accented(data);
        spark.set_look(filled.unwrap_or(true), smooth, leading_dot);
        if let Some(max) = max {
            spark = spark.with_max(max as f32);
        }
        spark.label = label;
        spark.tone = tone;
        spark
    }

    /// A changed sample set changes the line; a changed tone is painted with the theme, so the widget
    /// stays and only what it wears changes.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a trend sample, well inside f32"
    )]
    fn set_props(&mut self, props: SparklineProps) -> bool {
        self.set_data(props.samples.iter().map(|sample| *sample as f32).collect());
        self.max = props.max.map(|max| max as f32);
        self.set_look(
            props.filled.unwrap_or(true),
            props.smooth,
            props.leading_dot,
        );
        self.label = props.label;
        self.tone = props.tone;
        true
    }
}
