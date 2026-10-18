//! Unit conversions typed into search: "10 km in mi", "5 kg to lb",
//! "100 °F", "3 GB in MiB". A quantity with no target converts to the other
//! system's usual unit (kilometres to miles, Celsius to Fahrenheit).
//!
//! Everything here is arithmetic over a fixed table; nothing is fetched.

/// What a unit measures; two units convert only within one dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dimension {
    Length,
    Mass,
    Volume,
    Time,
    Data,
    Speed,
    Area,
    Temperature,
}

/// One unit: the names it is typed as, how it is written in an answer, what
/// it measures, its size in the dimension's base unit, and the unit a
/// quantity converts to when no target is typed.
struct Unit {
    names: &'static [&'static str],
    symbol: &'static str,
    dimension: Dimension,
    factor: f64,
    default_to: Option<&'static str>,
}

const fn unit(
    names: &'static [&'static str],
    symbol: &'static str,
    dimension: Dimension,
    factor: f64,
    default_to: Option<&'static str>,
) -> Unit {
    Unit {
        names,
        symbol,
        dimension,
        factor,
        default_to,
    }
}

use Dimension::{Area, Data, Length, Mass, Speed, Temperature, Time, Volume};

const UNITS: &[Unit] = &[
    // Length, in metres.
    unit(
        &[
            "mm",
            "millimeter",
            "millimeters",
            "millimetre",
            "millimetres",
            "millimeter",
        ],
        "mm",
        Length,
        0.001,
        Some("in"),
    ),
    unit(
        &[
            "cm",
            "centimeter",
            "centimeters",
            "centimetre",
            "centimetres",
            "zentimeter",
        ],
        "cm",
        Length,
        0.01,
        Some("in"),
    ),
    unit(
        &["m", "meter", "meters", "metre", "metres"],
        "m",
        Length,
        1.0,
        Some("ft"),
    ),
    unit(
        &["km", "kilometer", "kilometers", "kilometre", "kilometres"],
        "km",
        Length,
        1000.0,
        Some("mi"),
    ),
    unit(
        &["in", "inch", "inches", "zoll", "\""],
        "in",
        Length,
        0.0254,
        Some("cm"),
    ),
    unit(
        &["ft", "foot", "feet", "fuß", "fuss", "'"],
        "ft",
        Length,
        0.3048,
        Some("m"),
    ),
    unit(&["yd", "yard", "yards"], "yd", Length, 0.9144, Some("m")),
    unit(
        &["mi", "mile", "miles", "meile", "meilen"],
        "mi",
        Length,
        1609.344,
        Some("km"),
    ),
    unit(
        &[
            "nmi",
            "nautical mile",
            "nautical miles",
            "seemeile",
            "seemeilen",
        ],
        "nmi",
        Length,
        1852.0,
        Some("km"),
    ),
    // Mass, in kilograms.
    unit(
        &["mg", "milligram", "milligrams", "milligramm"],
        "mg",
        Mass,
        1e-6,
        None,
    ),
    unit(
        &["g", "gram", "grams", "gramm"],
        "g",
        Mass,
        0.001,
        Some("oz"),
    ),
    unit(
        &["kg", "kilogram", "kilograms", "kilogramm", "kilo", "kilos"],
        "kg",
        Mass,
        1.0,
        Some("lb"),
    ),
    unit(
        &["t", "tonne", "tonnes", "ton", "tons"],
        "t",
        Mass,
        1000.0,
        None,
    ),
    unit(
        &["oz", "ounce", "ounces", "unze", "unzen"],
        "oz",
        Mass,
        0.028_349_523_125,
        Some("g"),
    ),
    unit(
        &["lb", "lbs", "pound", "pounds", "pfund"],
        "lb",
        Mass,
        0.453_592_37,
        Some("kg"),
    ),
    unit(
        &["st", "stone", "stones"],
        "st",
        Mass,
        6.350_293_18,
        Some("kg"),
    ),
    // Volume, in litres (US customary units).
    unit(
        &[
            "ml",
            "milliliter",
            "milliliters",
            "millilitre",
            "millilitres",
        ],
        "ml",
        Volume,
        0.001,
        Some("fl oz"),
    ),
    unit(
        &["cl", "centiliter", "centilitre"],
        "cl",
        Volume,
        0.01,
        None,
    ),
    unit(&["dl", "deciliter", "decilitre"], "dl", Volume, 0.1, None),
    unit(
        &["l", "liter", "liters", "litre", "litres"],
        "l",
        Volume,
        1.0,
        Some("gal"),
    ),
    unit(
        &["gal", "gallon", "gallons", "gallone", "gallonen"],
        "gal",
        Volume,
        3.785_411_784,
        Some("l"),
    ),
    unit(
        &["qt", "quart", "quarts"],
        "qt",
        Volume,
        0.946_352_946,
        Some("l"),
    ),
    unit(
        &["pt", "pint", "pints"],
        "pt",
        Volume,
        0.473_176_473,
        Some("ml"),
    ),
    unit(
        &["cup", "cups", "tasse", "tassen"],
        "cup",
        Volume,
        0.236_588_236_5,
        Some("ml"),
    ),
    unit(
        &["fl oz", "floz", "fluid ounce", "fluid ounces"],
        "fl oz",
        Volume,
        0.029_573_529_562_5,
        Some("ml"),
    ),
    unit(
        &["tbsp", "tablespoon", "tablespoons", "el", "esslöffel"],
        "tbsp",
        Volume,
        0.014_786_764_781_25,
        Some("ml"),
    ),
    unit(
        &["tsp", "teaspoon", "teaspoons", "tl", "teelöffel"],
        "tsp",
        Volume,
        0.004_928_921_593_75,
        Some("ml"),
    ),
    // Time, in seconds.
    unit(
        &[
            "ms",
            "millisecond",
            "milliseconds",
            "millisekunde",
            "millisekunden",
        ],
        "ms",
        Time,
        0.001,
        None,
    ),
    unit(
        &[
            "s", "sec", "secs", "second", "seconds", "sekunde", "sekunden",
        ],
        "s",
        Time,
        1.0,
        None,
    ),
    unit(
        &["min", "mins", "minute", "minutes", "minuten"],
        "min",
        Time,
        60.0,
        None,
    ),
    unit(
        &[
            "h", "hr", "hrs", "hour", "hours", "stunde", "stunden", "std",
        ],
        "h",
        Time,
        3600.0,
        None,
    ),
    unit(
        &["d", "day", "days", "tag", "tage"],
        "d",
        Time,
        86_400.0,
        None,
    ),
    unit(
        &["wk", "week", "weeks", "woche", "wochen"],
        "wk",
        Time,
        604_800.0,
        None,
    ),
    unit(
        &["yr", "year", "years", "jahr", "jahre"],
        "yr",
        Time,
        31_557_600.0,
        None,
    ),
    // Data, in bytes.
    unit(&["bit", "bits"], "bit", Data, 0.125, None),
    unit(&["byte", "bytes"], "B", Data, 1.0, None),
    unit(&["kb", "kilobyte", "kilobytes"], "kB", Data, 1e3, None),
    unit(&["mb", "megabyte", "megabytes"], "MB", Data, 1e6, None),
    unit(
        &["gb", "gigabyte", "gigabytes"],
        "GB",
        Data,
        1e9,
        Some("GiB"),
    ),
    unit(
        &["tb", "terabyte", "terabytes"],
        "TB",
        Data,
        1e12,
        Some("TiB"),
    ),
    unit(&["kib", "kibibyte", "kibibytes"], "KiB", Data, 1024.0, None),
    unit(
        &["mib", "mebibyte", "mebibytes"],
        "MiB",
        Data,
        1_048_576.0,
        None,
    ),
    unit(
        &["gib", "gibibyte", "gibibytes"],
        "GiB",
        Data,
        1_073_741_824.0,
        Some("GB"),
    ),
    unit(
        &["tib", "tebibyte", "tebibytes"],
        "TiB",
        Data,
        1_099_511_627_776.0,
        Some("TB"),
    ),
    // Speed, in metres per second.
    unit(&["m/s", "mps"], "m/s", Speed, 1.0, Some("km/h")),
    unit(
        &["km/h", "kmh", "kph", "kmph"],
        "km/h",
        Speed,
        1.0 / 3.6,
        Some("mph"),
    ),
    unit(&["mph"], "mph", Speed, 0.447_04, Some("km/h")),
    unit(
        &["kn", "knot", "knots", "knoten"],
        "kn",
        Speed,
        0.514_444_444_444_444_5,
        Some("km/h"),
    ),
    // Area, in square metres.
    unit(
        &[
            "m2",
            "m²",
            "sqm",
            "square meter",
            "square meters",
            "square metre",
            "square metres",
            "quadratmeter",
        ],
        "m²",
        Area,
        1.0,
        Some("ft²"),
    ),
    unit(
        &[
            "km2",
            "km²",
            "square kilometer",
            "square kilometers",
            "quadratkilometer",
        ],
        "km²",
        Area,
        1e6,
        Some("mi²"),
    ),
    unit(&["cm2", "cm²"], "cm²", Area, 1e-4, None),
    unit(
        &["ha", "hectare", "hectares", "hektar"],
        "ha",
        Area,
        1e4,
        Some("acre"),
    ),
    unit(
        &["acre", "acres"],
        "acre",
        Area,
        4_046.856_422_4,
        Some("ha"),
    ),
    unit(
        &["ft2", "ft²", "sqft", "square foot", "square feet"],
        "ft²",
        Area,
        0.092_903_04,
        Some("m²"),
    ),
    unit(
        &["mi2", "mi²", "square mile", "square miles"],
        "mi²",
        Area,
        2_589_988.110_336,
        Some("km²"),
    ),
    // Temperature; converted by `kelvin` and `from_kelvin`, not by factor.
    unit(
        &["c", "°c", "celsius", "degc", "grad"],
        "°C",
        Temperature,
        1.0,
        Some("°F"),
    ),
    unit(
        &["f", "°f", "fahrenheit", "degf"],
        "°F",
        Temperature,
        1.0,
        Some("°C"),
    ),
    unit(&["k", "kelvin"], "K", Temperature, 1.0, Some("°C")),
];

/// The unit typed as `name`, by any of its names or its symbol.
fn find(name: &str) -> Option<&'static Unit> {
    let name = name.trim().to_lowercase();
    if name.is_empty() {
        return None;
    }
    UNITS
        .iter()
        .find(|unit| unit.names.contains(&name.as_str()) || unit.symbol.to_lowercase() == name)
}

fn kelvin(value: f64, unit: &Unit) -> f64 {
    match unit.symbol {
        "°C" => value + 273.15,
        "°F" => (value - 32.0) * 5.0 / 9.0 + 273.15,
        _ => value,
    }
}

fn from_kelvin(value: f64, unit: &Unit) -> f64 {
    match unit.symbol {
        "°C" => value - 273.15,
        "°F" => (value - 273.15) * 9.0 / 5.0 + 32.0,
        _ => value,
    }
}

/// The words that separate a quantity from the unit it should be given in.
const INTO: &[&str] = &[
    " in ", " to ", " into ", " as ", " -> ", "->", " = ", " nach ", " zu ", " als ",
];

/// The leading number of `text` (a decimal point or comma) and the rest.
fn leading_number(text: &str) -> Option<(f64, &str)> {
    let end = text
        .char_indices()
        .find(|(at, c)| !(c.is_ascii_digit() || *c == '.' || *c == ',' || (*at == 0 && *c == '-')))
        .map_or(text.len(), |(at, _)| at);
    let number = text.get(..end)?;
    if !number.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let value: f64 = number.replace(',', ".").parse().ok()?;
    Some((value, text.get(end..)?))
}

/// One conversion: the quantity as typed and as answered.
#[derive(Debug, PartialEq)]
pub(crate) struct Conversion {
    pub from: String,
    pub to: String,
}

/// `typed` as a conversion, if it is a number with a unit, and optionally
/// the unit to convert to.
pub(crate) fn convert(typed: &str) -> Option<Conversion> {
    let text = typed.trim().to_lowercase();
    let (value, rest) = leading_number(&text)?;
    let rest = format!(" {} ", rest.trim());
    let (from_name, to_name) = INTO
        .iter()
        .find_map(|word| rest.split_once(word))
        .map_or((rest.as_str(), None), |(from, to)| (from, Some(to)));
    let from = find(from_name)?;
    let to = match to_name {
        Some(name) => find(name)?,
        None => find(from.default_to?)?,
    };
    if from.dimension != to.dimension || std::ptr::eq(from, to) {
        return None;
    }
    let answer = if from.dimension == Temperature {
        from_kelvin(kelvin(value, from), to)
    } else {
        value * from.factor / to.factor
    };
    answer.is_finite().then(|| Conversion {
        from: format!("{} {}", measure(value), from.symbol),
        to: format!("{} {}", measure(answer), to.symbol),
    })
}

/// A measure as a person reads it: four decimals at most, fewer when they
/// are zeros, and significant digits kept for very small values.
fn measure(value: f64) -> String {
    let magnitude = value.abs();
    let text = if magnitude > 0.0 && magnitude < 0.001 {
        format!("{value:.3e}")
    } else {
        let text = format!("{value:.4}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    };
    if text == "-0" { "0".to_owned() } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(typed: &str) -> Option<String> {
        convert(typed).map(|conversion| conversion.to)
    }

    #[test]
    fn quantities_convert_within_their_dimension() {
        assert_eq!(answer("10 km in mi").as_deref(), Some("6.2137 mi"));
        assert_eq!(answer("5 kg to lb").as_deref(), Some("11.0231 lb"));
        assert_eq!(answer("100 °F").as_deref(), Some("37.7778 °C"));
        assert_eq!(answer("0 c in f").as_deref(), Some("32 °F"));
        assert_eq!(answer("1 GB in MiB").as_deref(), Some("953.6743 MiB"));
        assert_eq!(answer("2,5 l").as_deref(), Some("0.6604 gal"));
        assert_eq!(answer("90 min in h").as_deref(), Some("1.5 h"));
        assert_eq!(answer("100 km/h").as_deref(), Some("62.1371 mph"));
        assert_eq!(answer("3 fuß nach cm").as_deref(), Some("91.44 cm"));
    }

    #[test]
    fn nothing_that_is_not_a_quantity_converts() {
        assert_eq!(answer("10 km in kg"), None, "different dimensions");
        assert_eq!(answer("10"), None);
        assert_eq!(answer("notes"), None);
        assert_eq!(answer("10 apples"), None);
        assert_eq!(answer("5 s"), None, "no usual target for seconds");
        assert_eq!(answer("1 km in km"), None);
    }
}
