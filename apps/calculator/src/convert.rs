//! Units, and the arithmetic that turns one into another, entirely offline.
//!
//! A calculator that has a functions page and a graph still cannot answer
//! "40 °C in °F", which is one of the things people reach for a calculator
//! for most. This module holds the quantity table and the conversion; the
//! page that shows it is [`crate::convert_page`].
//!
//! Everything here is arithmetic over a fixed table: nothing is fetched, so a
//! conversion works with the network off and in a room with no server. Money
//! is the one quantity Tilo cannot convert honestly — a rate is a fact about
//! the moment, not a definition — so currency is a category that says so
//! ([`Dimension::Currency::rates_note`]) rather than a table of stale numbers.

/// A unit: how it is written, what it is called, and how to reach the
/// quantity's base unit and come back.
#[derive(Clone, Copy, Debug)]
pub struct Unit {
    /// The symbol the page shows and a converted result carries: `°C`, `km`,
    /// `m/s`.
    pub symbol: &'static str,
    /// The unit in words, for assistive technology and the page's title.
    pub name: &'static str,
    to_base: fn(f64) -> f64,
    from_base: fn(f64) -> f64,
}

/// What a quantity measures. Two values convert only within one dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    Length,
    Mass,
    Volume,
    Time,
    Speed,
    Area,
    Data,
    Temperature,
}

/// The quantities the page offers, in the order it offers them.
pub const DIMENSIONS: [Dimension; 8] = [
    Dimension::Length,
    Dimension::Mass,
    Dimension::Volume,
    Dimension::Temperature,
    Dimension::Time,
    Dimension::Speed,
    Dimension::Area,
    Dimension::Data,
];

impl Dimension {
    /// The quantity's name: the chip's word and the page's heading.
    pub fn label(self) -> &'static str {
        match self {
            Self::Length => "Length",
            Self::Mass => "Mass",
            Self::Volume => "Volume",
            Self::Time => "Time",
            Self::Speed => "Speed",
            Self::Area => "Area",
            Self::Data => "Data",
            Self::Temperature => "Temperature",
        }
    }

    /// A stable slug for the semantic addresses of this quantity's controls.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Length => "length",
            Self::Mass => "mass",
            Self::Volume => "volume",
            Self::Time => "time",
            Self::Speed => "speed",
            Self::Area => "area",
            Self::Data => "data",
            Self::Temperature => "temperature",
        }
    }

    /// The units it offers, in a person's order: the small ones first.
    pub fn units(self) -> &'static [Unit] {
        match self {
            Self::Length => &LENGTH,
            Self::Mass => &MASS,
            Self::Volume => &VOLUME,
            Self::Time => &TIME,
            Self::Speed => &SPEED,
            Self::Area => &AREA,
            Self::Data => &DATA,
            Self::Temperature => &TEMPERATURE,
        }
    }

    /// The units the page starts with: the metric one people mean, and the
    /// one they usually want it in. Lengths: metres to feet. Mass: kilograms
    /// to pounds. Temperatures: Celsius to Fahrenheit.
    pub fn default_pair(self) -> (usize, usize) {
        match self {
            Self::Length => (2, 5),
            Self::Mass => (1, 3),
            Self::Volume => (0, 3),
            Self::Time => (1, 0),
            Self::Speed | Self::Data | Self::Temperature => (0, 1),
            Self::Area => (0, 2),
        }
    }
}

/// The unit a page falls back to when the quantity it names has none: a
/// plain metre, so reading and drawing never index an empty table.
pub const FALLBACK_UNIT: Unit = Unit {
    symbol: "m",
    name: "metres",
    to_base: |value| value,
    from_base: |value| value,
};

/// A conversion from one unit to another within one quantity.
pub fn convert(value: f64, from: &Unit, to: &Unit) -> f64 {
    (to.from_base)((from.to_base)(value))
}

/// The table: each unit with its factor into the quantity's base unit
/// (metres, kilograms, litres, seconds, metres per second, square metres,
/// bytes) and the offsets temperature needs.
macro_rules! scaled {
    ($symbol:literal, $name:literal, $factor:expr) => {
        Unit {
            symbol: $symbol,
            name: $name,
            to_base: |v| v * $factor,
            from_base: |v| v / $factor,
        }
    };
}

static LENGTH: [Unit; 9] = [
    scaled!("mm", "millimetres", 0.001),
    scaled!("cm", "centimetres", 0.01),
    scaled!("m", "metres", 1.0),
    scaled!("km", "kilometres", 1000.0),
    scaled!("in", "inches", 0.0254),
    scaled!("ft", "feet", 0.3048),
    scaled!("yd", "yards", 0.9144),
    scaled!("mi", "miles", 1609.344),
    scaled!("nmi", "nautical miles", 1852.0),
];

static MASS: [Unit; 6] = [
    scaled!("mg", "milligrams", 0.000_001),
    scaled!("g", "grams", 0.001),
    scaled!("kg", "kilograms", 1.0),
    scaled!("lb", "pounds", 0.453_592_37),
    scaled!("oz", "ounces", 0.028_349_523),
    scaled!("t", "tonnes", 1000.0),
];

static VOLUME: [Unit; 6] = [
    scaled!("ml", "millilitres", 0.001),
    scaled!("l", "litres", 1.0),
    scaled!("tsp", "teaspoons", 0.004_928_922),
    scaled!("tbsp", "tablespoons", 0.014_786_765),
    scaled!("fl oz", "fluid ounces", 0.028_413_063),
    scaled!("gal", "US gallons", 3.785_411_784),
];

static TIME: [Unit; 6] = [
    scaled!("s", "seconds", 1.0),
    scaled!("min", "minutes", 60.0),
    scaled!("h", "hours", 3600.0),
    scaled!("d", "days", 86_400.0),
    scaled!("wk", "weeks", 604_800.0),
    scaled!("yr", "years", 31_557_600.0),
];

static SPEED: [Unit; 4] = [
    scaled!("m/s", "metres per second", 1.0),
    scaled!("km/h", "kilometres per hour", 0.277_777_778),
    scaled!("mph", "miles per hour", 0.447_04),
    scaled!("kn", "knots", 0.514_444_444),
];

static AREA: [Unit; 5] = [
    scaled!("m²", "square metres", 1.0),
    scaled!("ha", "hectares", 10_000.0),
    scaled!("ft²", "square feet", 0.092_903_04),
    scaled!("ac", "acres", 4_046.856_422),
    scaled!("km²", "square kilometres", 1_000_000.0),
];

static DATA: [Unit; 6] = [
    scaled!("B", "bytes", 1.0),
    scaled!("kB", "kilobytes", 1000.0),
    scaled!("MB", "megabytes", 1_000_000.0),
    scaled!("GB", "gigabytes", 1_000_000_000.0),
    scaled!("KiB", "kibibytes", 1024.0),
    scaled!("GiB", "gibibytes", 1_073_741_824.0),
];

/// Temperature is the one quantity whose units do not share a zero: `40 °C`
/// is `104 °F`, and neither is a multiple of the other.
static TEMPERATURE: [Unit; 3] = [
    Unit {
        symbol: "°C",
        name: "degrees Celsius",
        to_base: |v| v + 273.15,
        from_base: |v| v - 273.15,
    },
    Unit {
        symbol: "°F",
        name: "degrees Fahrenheit",
        to_base: |v| (v + 459.67) * 5.0 / 9.0,
        from_base: |v| v * 9.0 / 5.0 - 459.67,
    },
    Unit {
        symbol: "K",
        name: "kelvin",
        to_base: |v| v,
        from_base: |v| v,
    },
];

/// What the page says where a currency conversion would be: Tilo has no
/// exchange-rate source, and a stale rate is worse than an honest refusal.
pub const CURRENCY_NOTE: &str = "A rate has to come from somewhere: Tilo has no exchange-rate source here, so this \
     page converts units and not money.";

#[cfg(test)]
mod tests {
    use super::*;

    fn length(symbol: &str) -> Unit {
        *LENGTH
            .iter()
            .find(|unit| unit.symbol == symbol)
            .expect("a length unit")
    }

    fn temperature(symbol: &str) -> Unit {
        *TEMPERATURE
            .iter()
            .find(|unit| unit.symbol == symbol)
            .expect("a temperature unit")
    }

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= expected.abs() * 1e-9 + 1e-9,
            "{actual} is not {expected}"
        );
    }

    #[test]
    fn the_answer_the_audit_asked_for() {
        close(convert(40.0, &temperature("°C"), &temperature("°F")), 104.0);
        close(convert(104.0, &temperature("°F"), &temperature("°C")), 40.0);
        close(convert(0.0, &temperature("°C"), &temperature("K")), 273.15);
        close(
            convert(-40.0, &temperature("°C"), &temperature("°F")),
            -40.0,
        );
    }

    #[test]
    fn lengths_masses_and_speeds_convert_both_ways() {
        close(convert(1.0, &length("km"), &length("mi")), 0.621_371_192);
        close(convert(1.0, &length("mi"), &length("km")), 1.609_344);
        close(convert(5.0, &length("ft"), &length("m")), 1.524);
        close(convert(1.0, &length("in"), &length("cm")), 2.54);
        let kg = *MASS.iter().find(|u| u.symbol == "kg").expect("kg");
        let lb = *MASS.iter().find(|u| u.symbol == "lb").expect("lb");
        close(convert(1.0, &kg, &lb), 2.204_622_622);
        close(convert(100.0, &SPEED[1], &SPEED[0]), 27.777_777_778);
    }

    #[test]
    fn converting_to_the_same_unit_changes_nothing() {
        for dimension in DIMENSIONS {
            for unit in dimension.units() {
                close(convert(12.5, unit, unit), 12.5);
            }
        }
    }

    #[test]
    fn every_default_pair_names_two_units_of_its_own_quantity() {
        for dimension in DIMENSIONS {
            let units = dimension.units();
            let (from, to) = dimension.default_pair();
            assert!(from < units.len() && to < units.len(), "{dimension:?}");
            assert_ne!(from, to, "{dimension:?}");
        }
    }

    #[test]
    fn every_quantity_has_a_name_and_a_slug_of_its_own() {
        let mut slugs: Vec<&str> = DIMENSIONS.iter().map(|d| d.slug()).collect();
        let count = slugs.len();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), count, "every slug is distinct");
        assert!(DIMENSIONS.iter().all(|d| !d.label().is_empty()));
    }
}

/// A converted amount as a person reads it: six significant digits, without
/// the trailing zeros a floating-point sum leaves behind. A conversion is an
/// approximate fact; printing sixteen digits of it claims a precision neither
/// the table nor the person typing has.
pub fn format_quantity(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    let magnitude = value.abs().log10().floor();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a decimal exponent of a finite f64 fits in i32"
    )]
    let magnitude = magnitude as i32;
    let decimals = (5 - magnitude).clamp(0, 12);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=12 just above"
    )]
    let decimals = decimals as usize;
    let text = format!("{value:.decimals$}");
    let text = match text.split_once('.') {
        Some((whole, fraction)) => {
            let fraction = fraction.trim_end_matches('0');
            if fraction.is_empty() {
                whole.to_owned()
            } else {
                format!("{whole}.{fraction}")
            }
        }
        None => text,
    };
    // "-0" is not a quantity anybody converts.
    if text == "-0" { "0".to_owned() } else { text }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn an_amount_keeps_six_significant_digits_and_no_trailing_zeros() {
        assert_eq!(format_quantity(40.0 / 0.3048), "131.234");
        assert_eq!(format_quantity(104.0), "104");
        assert_eq!(format_quantity(1.0 / 1.609_344), "0.621371");
        assert_eq!(format_quantity(1.0 / 3.0), "0.333333");
        assert_eq!(format_quantity(1.0 / 0.453_592_37), "2.20462");
        assert_eq!(format_quantity(0.0), "0");
        assert_eq!(format_quantity(-0.0), "0");
        assert_eq!(format_quantity(1.0e9), "1000000000");
        assert_eq!(format_quantity(f64::INFINITY), "—");
    }
}
