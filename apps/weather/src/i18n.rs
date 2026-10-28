//! Weather's copy, through the toolkit's Fluent catalogues.
//!
//! The catalogue is embedded with `include_str!`, so a live window, a
//! `--shots` capture and a unit test resolve the same strings with no file
//! I/O: the forecast's freshness line, the sections' names and the readings
//! assistive technology hears. The person's language comes from the usual
//! POSIX variables; a message a language lacks falls back to `en-US`.

use ui::i18n::ArgValue;

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// One named variable for [`t_args`], as `(name, value)`.
pub fn arg(name: &str, value: impl Into<ArgValue>) -> (&str, ArgValue) {
    (name, value.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The German catalogue ships with the app, so a person reading German
    /// reads the forecast's own copy in German too.
    #[test]
    fn the_german_catalogue_words_the_forecast_pages_copy() {
        CATALOG.set_locale("de-DE");
        assert_eq!(t("weather-hours"), "Nächste 24 Stunden");
        assert_eq!(
            t_args(
                "weather-hour-row-rain",
                &[
                    arg("temperature", "7°"),
                    arg("sky", "Klar"),
                    arg("chance", "20%"),
                ],
            ),
            "7°, Klar, 20% Niederschlagsrisiko"
        );
        CATALOG.set_locale("en-US");
        assert_eq!(t("weather-hours"), "Next 24 hours");
    }
}
