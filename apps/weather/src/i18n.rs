//! Weather's copy, through the toolkit's Fluent catalogues. The catalogue is
//! embedded, so a live window, a capture and a unit test resolve the same
//! strings with no file IO. A message a language lacks falls back to `en-US`.

use ui::i18n::ArgValue;

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

pub fn arg(name: &str, value: impl Into<ArgValue>) -> (&str, ArgValue) {
    (name, value.into())
}

#[cfg(test)]
mod tests {
    use super::*;

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
