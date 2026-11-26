//! [`ProgressBar`] as a described component.

use crate::widget::GaugeThresholds;
use tilo_ui_base::widget::Tone;
use tilo_ui_base::{component::Component, component::Described};

use super::{BarFill, ProgressBar};

/// How far something is (a transfer, an import, a sync): without a value a segment slides
/// across the track with no known end, and a measured level (a disk, a quota) turns warning
/// then danger as it fills.
#[derive(Component, Default)]
#[component(name = "ProgressBar", role = "ProgressBar", label = "label")]
pub struct ProgressBarProps {
    /// The share done, from 0 to 1; without it the bar has no known end.
    #[component(positional)]
    pub value: Option<f64>,
    /// What is under way ("Uploading"), shown over the track; a named bar announces its
    /// progress without taking focus.
    pub label: Option<String>,
    /// How assistive technology names it where no label shows: a step track under a heading that
    /// already says the step is named by the whole stage ("Identity setup, step 2 of 4: Protect")
    /// without repeating it over the track. A shown label names it otherwise.
    pub accessible_name: Option<String>,
    /// A status the fill means: a failed transfer stays danger.
    pub tone: Option<Tone>,
    /// A measured level instead of progress: the fill turns warning, then danger, as it fills.
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
        match (props.label, props.accessible_name) {
            (Some(label), _) => bar.with_shown_label(label),
            (None, Some(name)) => bar.with_label(name),
            (None, None) => bar,
        }
    }

    fn set_props(&mut self, props: ProgressBarProps) -> bool {
        let fill = props.fill();
        if props.value.is_none() != self.indeterminate || fill != self.fill {
            return false;
        }
        if let Some(value) = props.value {
            self.set_progress(share(value));
        }
        if props.label.is_some() != self.label_shown {
            return false;
        }
        self.label = props.label.or(props.accessible_name);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{ProgressBar, ProgressBarProps};
    use crate::widget::Widget;
    use tilo_ui_base::component::Described;

    fn bar(label: Option<&str>, accessible_name: Option<&str>) -> ProgressBar {
        ProgressBar::from_props(
            "bar",
            ProgressBarProps {
                value: Some(0.5),
                label: label.map(str::to_owned),
                accessible_name: accessible_name.map(str::to_owned),
                ..ProgressBarProps::default()
            },
        )
    }

    fn name(bar: &ProgressBar) -> Option<String> {
        Widget::accessibility(bar).and_then(|node| node.name)
    }

    #[test]
    fn a_bar_named_only_for_assistive_technology_shows_no_label_and_follows_its_stage() {
        let mut quiet = bar(None, Some("Setup, step 2 of 4: Protect"));
        assert_eq!(name(&quiet).as_deref(), Some("Setup, step 2 of 4: Protect"));
        assert!(!quiet.label_shown, "the name is not drawn over the track");

        assert!(quiet.set_props(ProgressBarProps {
            value: Some(0.75),
            accessible_name: Some("Setup, step 3 of 4: Backup".to_owned()),
            ..ProgressBarProps::default()
        }));
        assert_eq!(name(&quiet).as_deref(), Some("Setup, step 3 of 4: Backup"));

        let shown = bar(Some("Uploading"), Some("ignored while a label shows"));
        assert_eq!(name(&shown).as_deref(), Some("Uploading"));
        assert!(shown.label_shown);
        assert_eq!(name(&bar(None, None)), None, "an unnamed bar stays unnamed");
    }
}
