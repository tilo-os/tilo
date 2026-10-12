//! Histogram construction from raw samples.

use vello::peniko::Color;

use super::{
    AxisRange, CartesianPlot, CartesianStyle, PlotAxis, PlotDatum, PlotSeries, SeriesKind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistogramNormalization {
    Count,
    Probability,
    Density,
}

/// Fluent builder that bins raw samples and returns a normal
/// [`CartesianPlot`], retaining all of its axis, tooltip and activation APIs.
pub struct Histogram {
    samples: Vec<f64>,
    bins: usize,
    range: Option<(f64, f64)>,
    normalization: HistogramNormalization,
    color: Color,
    title: String,
    series_name: String,
    bar_gap: f64,
}

impl Histogram {
    pub fn new(samples: impl IntoIterator<Item = f64>, color: Color) -> Self {
        Self {
            samples: samples
                .into_iter()
                .filter(|value| value.is_finite())
                .collect(),
            bins: 12,
            range: None,
            normalization: HistogramNormalization::Count,
            color,
            title: String::new(),
            series_name: "Frequency".into(),
            bar_gap: 3.0,
        }
    }

    #[must_use]
    pub fn bins(mut self, bins: usize) -> Self {
        self.bins = bins.clamp(1, 512);
        self
    }

    #[must_use]
    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.range = (max > min && min.is_finite() && max.is_finite()).then_some((min, max));
        self
    }

    #[must_use]
    pub fn normalize(mut self, normalization: HistogramNormalization) -> Self {
        self.normalization = normalization;
        self
    }

    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.series_name = name.into();
        self
    }

    #[must_use]
    pub fn bar_gap(mut self, gap: f64) -> Self {
        self.bar_gap = gap.max(0.0);
        self
    }

    pub fn build(self) -> CartesianPlot {
        let (min, max) = self.resolved_range();
        let bin_count = u32::try_from(self.bins).unwrap_or(u32::MAX);
        let width = (max - min) / f64::from(bin_count);
        let mut counts = vec![0_u64; self.bins];
        for sample in &self.samples {
            if *sample < min || *sample > max {
                continue;
            }
            let fraction = ((*sample - min) / (max - min)).clamp(0.0, 1.0);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped fraction multiplied by a bounded bin count"
            )]
            let mut index = (fraction * f64::from(bin_count)).floor() as usize;
            if index == self.bins {
                index = index.saturating_sub(1);
            }
            if let Some(count) = counts.get_mut(index) {
                *count = count.saturating_add(1);
            }
        }
        let sample_count =
            f64::from(u32::try_from(self.samples.len()).unwrap_or(u32::MAX)).max(1.0);
        let data = counts
            .into_iter()
            .enumerate()
            .map(|(index, count)| {
                let index_f64 = f64::from(u32::try_from(index).unwrap_or(u32::MAX));
                let low = min + index_f64 * width;
                let high = low + width;
                let count_f64 = f64::from(u32::try_from(count).unwrap_or(u32::MAX));
                let value = match self.normalization {
                    HistogramNormalization::Count => count_f64,
                    HistogramNormalization::Probability => count_f64 / sample_count,
                    HistogramNormalization::Density => count_f64 / sample_count / width,
                };
                PlotDatum::new(format!("bin-{index}"), low.midpoint(high), value)
                    .labelled(format!("{low:.2} – {high:.2}"))
                    .with_detail(format!("{count} observations"))
            })
            .collect();
        // Cartesian bars are sized in logical pixels. Use a representative
        // 720 px plot width; the plot remains responsive and the explicit gap
        // stays visually stable instead of changing with the sample domain.
        let bar_width = (720.0 / f64::from(bin_count) - self.bar_gap).max(2.0);
        let series = PlotSeries::new("histogram", self.series_name, self.color, data)
            .kind(SeriesKind::Bar { width: bar_width });
        let style = CartesianStyle {
            x_axis: PlotAxis {
                range: AxisRange::Fixed { min, max },
                ..Default::default()
            },
            y_axis: PlotAxis {
                range: AxisRange::Auto,
                include_zero: true,
                ..Default::default()
            },
            ..Default::default()
        };
        CartesianPlot::new(vec![series])
            .with_style(style)
            .titled(self.title)
    }

    fn resolved_range(&self) -> (f64, f64) {
        if let Some(range) = self.range {
            return range;
        }
        let (min, max) = self
            .samples
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
                (min.min(*value), max.max(*value))
            });
        if min.is_finite() && max > min {
            (min, max)
        } else {
            (min.min(0.0) - 0.5, min.min(0.0) + 0.5)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_builder_handles_empty_and_constant_samples() {
        fn constant_samples_get_a_nonzero_range() {
            let histogram = Histogram::new([4.0, 4.0], Color::WHITE);
            let (min, max) = histogram.resolved_range();
            assert!(max > min);
        }

        fn builder_accepts_empty_samples() {
            let mut plot = Histogram::new([], Color::WHITE).bins(8).build();
            assert!(plot.take_activated().is_none());
        }

        constant_samples_get_a_nonzero_range();
        builder_accepts_empty_samples();
    }
}
