//! Units and the arithmetic that turns one into another, from a fixed table:
//! nothing is fetched, so a conversion works with the network off. Money is the
//! one quantity Tilo cannot convert honestly, since a rate is a fact about the
//! moment, so currency says why instead of holding stale numbers.

/// How a unit is written and called, and how it reaches the quantity's base
/// unit and back.
#[derive(Clone, Copy, Debug)]
pub struct Unit {
    pub symbol: &'static str,
    pub name: &'static str,
    to_base: fn(f64) -> f64,
    from_base: fn(f64) -> f64,
}

/// Two values convert only within one dimension.
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
    pub fn label(self) -> String {
        let message = match self {
            Self::Length => "calculator-quantity-length",
            Self::Mass => "calculator-quantity-mass",
            Self::Volume => "calculator-quantity-volume",
            Self::Time => "calculator-quantity-time",
            Self::Speed => "calculator-quantity-speed",
            Self::Area => "calculator-quantity-area",
            Self::Data => "calculator-quantity-data",
            Self::Temperature => "calculator-quantity-temperature",
        };
        crate::resource::CATALOG.t(message)
    }

    /// Stable: the semantic addresses of its controls embed it.
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

    /// In a person's order: the small ones first.
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

    /// Lengths: metres to feet. Mass: kilograms to pounds. Temperatures:
    /// Celsius to Fahrenheit.
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

/// A plain metre, so reading and drawing never index an empty table.
pub const FALLBACK_UNIT: Unit = Unit {
    symbol: "m",
    name: "metres",
    to_base: |value| value,
    from_base: |value| value,
};

pub fn convert(value: f64, from: &Unit, to: &Unit) -> f64 {
    (to.from_base)((from.to_base)(value))
}

/// Each unit with its factor into the quantity's base unit, and the offsets
/// temperature needs.
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

/// Temperature's units do not share a zero: `40 °C` is `104 °F`.
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

/// Tilo has no exchange-rate source, and a stale rate is worse than an honest
/// refusal.
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

/// Six significant digits, without the trailing zeros a float sum leaves. A
/// conversion is approximate: sixteen digits claim a precision nobody has.
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
