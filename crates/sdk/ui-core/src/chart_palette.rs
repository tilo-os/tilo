//! Theme-aware reusable colour palettes for charts and data visualisation.

use crate::animation::Lerp;
use crate::num;
use crate::theme::Theme;
use vello::peniko::Color;

#[derive(Clone, Debug)]
pub struct ChartPalette {
    categorical: Vec<Color>,
    sequential: (Color, Color),
    diverging: (Color, Color, Color),
}

impl ChartPalette {
    /// Build a non-empty custom palette. Requiring the primary colour makes an
    /// empty palette unrepresentable and keeps indexed lookup total.
    pub fn new(primary: Color, additional: impl IntoIterator<Item = Color>) -> Self {
        let mut categorical = vec![primary];
        categorical.extend(additional);
        let last = categorical.last().copied().unwrap_or(primary);
        Self {
            categorical,
            sequential: (primary.multiply_alpha(0.28), primary),
            diverging: (primary, <Color as Lerp>::lerp(primary, last, 0.5), last),
        }
    }

    /// A palette derived from live design tokens, including semantic colours
    /// selected by light, dark, and high-contrast themes.
    pub fn from_theme(theme: &Theme) -> Self {
        let colors = &theme.tokens.colors;
        let series = &colors.presence;
        Self::new(
            series.series(0),
            (1..crate::tokens::colors::PRESENCE_LEN).map(|index| series.series(index)),
        )
        .with_sequential(colors.neutral.stops[2], series.series(0))
        .with_diverging(
            colors.semantic.danger,
            colors.neutral.stops[4],
            colors.semantic.success,
        )
    }

    #[must_use]
    pub fn with_sequential(mut self, low: Color, high: Color) -> Self {
        self.sequential = (low, high);
        self
    }

    #[must_use]
    pub fn with_diverging(mut self, negative: Color, midpoint: Color, positive: Color) -> Self {
        self.diverging = (negative, midpoint, positive);
        self
    }

    pub fn len(&self) -> usize {
        self.categorical.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Stable wraparound lookup, suitable for an arbitrary number of series.
    pub fn categorical(&self, index: usize) -> Color {
        let wrapped = index
            .checked_rem(self.categorical.len())
            .unwrap_or_default();
        self.categorical
            .get(wrapped)
            .copied()
            .unwrap_or(self.diverging.0)
    }

    /// Sample the sequential ramp. Values outside `0..=1` are clamped.
    pub fn sequential(&self, value: f64) -> Color {
        let value = num::f32_of_f64(value.clamp(0.0, 1.0));
        <Color as Lerp>::lerp(self.sequential.0, self.sequential.1, value)
    }

    /// Sample a diverging ramp where -1 is negative, 0 is the midpoint, and
    /// +1 is positive. Values outside that range are clamped.
    pub fn diverging(&self, value: f64) -> Color {
        let value = value.clamp(-1.0, 1.0);
        if value < 0.0 {
            <Color as Lerp>::lerp(self.diverging.1, self.diverging.0, num::f32_of_f64(-value))
        } else {
            <Color as Lerp>::lerp(self.diverging.1, self.diverging.2, num::f32_of_f64(value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_six_categories_are_distinct_hues() {
        use crate::color::Oklch;
        for theme in [Theme::dark(), Theme::light()] {
            let palette = ChartPalette::from_theme(&theme);
            let hues: Vec<f32> = (0..6)
                .map(|i| Oklch::from_color(palette.categorical(i)).h)
                .collect();
            for (i, a) in hues.iter().enumerate() {
                for b in hues.iter().skip(i + 1) {
                    let d = (a - b).rem_euclid(360.0);
                    let d = d.min(360.0 - d);
                    assert!(
                        d >= 55.0,
                        "{}: hues {a} and {b} are only {d}° apart",
                        theme.name
                    );
                }
            }
        }
    }

    #[test]
    fn chart_palettes_wrap_categories_and_clamp_ramps() {
        // Case `categorical_lookup_wraps_and_can_never_be_empty`.
        {
            let first = Color::from_rgb8(1, 2, 3);
            let second = Color::from_rgb8(4, 5, 6);
            let palette = ChartPalette::new(first, [second]);
            assert_eq!(palette.len(), 2);
            assert!(!palette.is_empty());
            assert_eq!(palette.categorical(0), first);
            assert_eq!(palette.categorical(2), first);
            assert_eq!(palette.categorical(usize::MAX), second);
        }

        // Case `continuous_ramps_clamp_and_preserve_endpoints`.
        {
            let low = Color::from_rgb8(10, 20, 30);
            let mid = Color::from_rgb8(100, 110, 120);
            let high = Color::from_rgb8(220, 230, 240);
            let palette = ChartPalette::new(low, [])
                .with_sequential(low, high)
                .with_diverging(low, mid, high);
            assert_eq!(palette.sequential(-10.0), low);
            assert_eq!(palette.sequential(10.0), high);
            assert_eq!(palette.diverging(-1.0), low);
            assert_eq!(palette.diverging(0.0), mid);
            assert_eq!(palette.diverging(1.0), high);
        }
    }
}
