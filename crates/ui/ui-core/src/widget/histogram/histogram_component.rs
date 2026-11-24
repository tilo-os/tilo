//! [`Histogram`] as a described component: raw samples binned into bars, which the builder hands
//! back as a [`CartesianPlot`].

use crate::component::{Component, Described};
use crate::widget::Decorator;

use super::{CartesianPlot, Histogram, HistogramNormalization};

/// How a bin's count reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistogramScale {
    /// How many samples fell in the bin.
    #[default]
    Count,
    /// The share of the samples that fell in the bin, from zero to one.
    Probability,
    /// The share of the samples per unit of the measured thing, which compares histograms of
    /// different bin widths.
    Density,
}

crate::component::choice!(HistogramScale, "HistogramScale", {
    Count => "count", Probability => "probability", Density => "density",
});

impl HistogramScale {
    fn normalization(self) -> HistogramNormalization {
        match self {
            Self::Count => HistogramNormalization::Count,
            Self::Probability => HistogramNormalization::Probability,
            Self::Density => HistogramNormalization::Density,
        }
    }
}

/// Raw observations as a distribution: how many fell in each bin, so a spread is read at a glance.
/// The bars take the theme's first palette colour and the axis is fitted to the data.
#[derive(Component, Default)]
#[component(name = "Histogram", role = "Image", label = "title")]
pub struct HistogramProps {
    /// The observations, in any order: `samples: [12.0, 18.0, 15.0, 22.0]`.
    #[component(required, positional)]
    pub samples: Vec<f64>,
    /// How many bins the range is cut into; twelve unless said.
    pub bins: Option<i64>,
    /// What a bin reads: its `count` unless said, a `probability`, or a `density`.
    pub scale: Option<HistogramScale>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
}

/// The plot `props` are drawn as.
fn plot(props: &HistogramProps) -> CartesianPlot {
    let mut histogram = Histogram::new(
        props.samples.iter().copied(),
        vello::peniko::Color::TRANSPARENT,
    )
    .palette(0)
    .normalize(props.scale.unwrap_or_default().normalization())
    .titled(props.title.clone().unwrap_or_default());
    if let Some(bins) = props.bins {
        histogram = histogram.bins(usize::try_from(bins.max(1)).unwrap_or(usize::MAX));
    }
    histogram.build()
}

/// The histogram as the toolkit's one chart shell, holding the plot its builder made.
pub struct HistogramChart {
    plot: CartesianPlot,
}

impl Decorator for HistogramChart {
    type Inner = CartesianPlot;

    fn inner(&self) -> &CartesianPlot {
        &self.plot
    }

    fn inner_mut(&mut self) -> &mut CartesianPlot {
        &mut self.plot
    }

    fn name(&self) -> &'static str {
        "Histogram"
    }
}

impl Described for HistogramChart {
    type Props = HistogramProps;

    fn from_props(_key: &str, props: HistogramProps) -> Self {
        Self { plot: plot(&props) }
    }

    /// The component stays while its plot is the one the new samples bin into: a histogram is
    /// binned anew, so it has no readings to keep inspecting.
    fn set_props(&mut self, props: HistogramProps) -> bool {
        self.plot = plot(&props);
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use super::*;
    use crate::chart_palette::ChartPalette;
    use crate::theme::Theme;

    #[test]
    fn samples_bin_into_bars_in_the_themes_first_palette_colour() {
        let chart = HistogramChart::from_props(
            "k",
            HistogramProps {
                samples: vec![1.0, 2.0, 2.5, 3.0, 3.5, 4.0],
                bins: Some(4),
                ..HistogramProps::default()
            },
        );
        assert_eq!(chart.plot.series().len(), 1);
        assert_eq!(chart.plot.series()[0].data.len(), 4, "four bins");
        for theme in [Theme::light(), Theme::dark()] {
            assert_eq!(
                chart.plot.series()[0].color_in(&theme),
                ChartPalette::from_theme(&theme).categorical(0)
            );
        }
    }

    #[test]
    fn changed_samples_rebin_where_they_stand() {
        let mut chart = HistogramChart::from_props(
            "k",
            HistogramProps {
                samples: vec![1.0, 2.0, 3.0],
                ..HistogramProps::default()
            },
        );
        assert_eq!(
            chart.plot.series()[0].data.len(),
            12,
            "twelve bins unless said"
        );
        assert!(chart.set_props(HistogramProps {
            samples: vec![1.0, 2.0, 3.0, 4.0],
            bins: Some(2),
            title: Some("Response time".to_owned()),
            ..HistogramProps::default()
        }));
        assert_eq!(chart.plot.series()[0].data.len(), 2);
        assert!(
            crate::widget::Widget::as_any(&chart).is_some(),
            "the widget answers its type in place"
        );
    }

    #[test]
    fn empty_samples_make_an_empty_histogram_rather_than_no_chart() {
        let chart = HistogramChart::from_props("k", HistogramProps::default());
        assert_eq!(chart.plot.series()[0].data.len(), 12);
    }
}
