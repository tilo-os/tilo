//! Tube's copy, through the toolkit's Fluent catalogues.
//!
//! The catalogue is embedded, so a live window, a `--shots` capture and a
//! test resolve the same strings with no file I/O. The person's locale comes
//! from the usual POSIX variables (`LC_ALL`, `LC_MESSAGES`, `LANGUAGE`,
//! `LANG`); a message a locale lacks falls back to `en-US`.

use ui::i18n::ArgValue;

ui::embedded_catalog!(pub fn, "tube-nav-home", ["en-US"]);

/// One named variable for [`t_args`].
pub fn arg<V: Into<ArgValue>>(name: &str, value: V) -> (&str, ArgValue) {
    (name, value.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");

    #[test]
    fn every_message_resolves_and_plurals_agree() {
        assert_eq!(t("tube-nav-home"), "Home");
        assert_eq!(
            t_args(
                "tube-video-count",
                &[arg("count", 1_u32), arg("shown", "1")]
            ),
            "1 video"
        );
        assert_eq!(
            t_args(
                "tube-video-count",
                &[arg("count", 12_u32), arg("shown", "12")]
            ),
            "12 videos"
        );
        for line in SOURCE_FTL.lines() {
            let Some((key, _)) = line.split_once(" = ") else {
                continue;
            };
            if key.starts_with('#') || key.starts_with(' ') || key.contains('.') {
                continue;
            }
            assert_ne!(t(key), key, "{key} resolves to copy");
        }
    }
}
