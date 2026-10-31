//! [`ProgressBar`] as a described component.

use crate::component::{Component, Described};
use crate::widget::{GaugeThresholds, Tone};

use super::{BarFill, ProgressBar};

/// How far something is: a transfer, an import, a sync. Without a value it is under way with no
/// known end, a segment sliding across the track. A measured level (a disk, a quota) turns
/// warning and then danger as it fills.
#[derive(Component, Default)]
#[component(name = "ProgressBar", role = "ProgressBar", label = "label")]
pub struct ProgressBarProps {
    /// The share done, from 0 to 1. Without it the bar is under way with no known end.
    #[component(positional)]
    pub value: Option<f64>,
    /// What is under way ("Uploading"). A named bar announces its progress to assistive
    /// technology without taking focus.
    pub label: Option<String>,
    /// A status the fill means: a failed transfer stays danger.
    pub tone: Option<Tone>,
    /// A measured level rather than progress: the fill turns warning, then danger, as it fills.
    pub level: bool,
    /// Where a level turns warning, from 0 to 1: 0.6 unless said.
    pub warning: Option<f64>,
    /// Where a level turns danger, from 0 to 1: 0.85 unless said.
    pub danger: Option<f64>,
}

impl ProgressBarProps {
    fn fill(&self) -> BarFill {
        match self.tone {
            Some(tone) => BarFill::Tone(tone),
            None if self.level => BarFill::Level(GaugeThresholds::given(self.warning, self.danger)),
            None => BarFill::Progress,
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "a share from 0 to 1, clamped by the bar"
)]
fn share(value: f64) -> f32 {
    value as f32
}

impl Described for ProgressBar {
    type Props = ProgressBarProps;

    fn from_props(_key: &str, props: ProgressBarProps) -> Self {
        let fill = props.fill();
        let mut bar = match props.value {
            Some(value) => Self::new(share(value)),
            None => Self::indeterminate(),
        };
        bar.fill = fill;
        bar.label = props.label;
        bar
    }

    /// A new share fills in place.
    fn set_props(&mut self, props: ProgressBarProps) -> bool {
        let fill = props.fill();
        if props.value.is_none() != self.indeterminate || fill != self.fill {
            return false;
        }
        if let Some(value) = props.value {
            self.set_progress(share(value));
        }
        self.label = props.label;
        true
    }
}
