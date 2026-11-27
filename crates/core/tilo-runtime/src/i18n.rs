//! The runtime's Fluent catalogue: the words it shows or exports itself
//! (the device consent prompt, why an agent does not reach another device), resolved in the
//! person's locale, `en-US` where a translation lacks a message.

#[cfg(target_os = "macos")]
use ui::i18n::ArgValue;

ui::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);

/// The arguments of a message about `app` and `device`: each catalogue
/// selects its own sentence on the device, since a spliced-in noun does not
/// agree with the words around it in every language.
#[cfg(target_os = "macos")]
pub(crate) fn device_args(
    app: &str,
    device: tilo_capture::DeviceKind,
) -> [(&'static str, ArgValue); 2] {
    [
        ("app", app.into()),
        ("device", crate::apps::device_cap(device).name().into()),
    ]
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::*;

    #[test]
    fn every_message_resolves_in_every_catalogue() {
        let keys: Vec<&str> = include_str!("../locales/en-US.ftl")
            .lines()
            .filter_map(|line| line.split_once(" = ").map(|(key, _)| key.trim()))
            .collect();
        for (locale, source) in [
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ] {
            for key in &keys {
                assert!(
                    source
                        .lines()
                        .any(|line| line.starts_with(&format!("{key} = "))),
                    "{locale} lacks {key}"
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_device_is_named_in_the_consent_prompt() {
        let camera = t_args(
            "device-consent-question",
            &device_args("Video", tilo_capture::DeviceKind::Camera),
        );
        let microphone = t_args(
            "device-grant-denied",
            &device_args("Video", tilo_capture::DeviceKind::Microphone),
        );
        assert_eq!(camera, "Allow Video to use the camera?");
        assert_eq!(microphone, "Video may not use the microphone");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn german_names_each_device_with_its_own_article() {
        let catalog = ui::i18n::EmbeddedCatalog::new(
            "de-DE",
            &[("de-DE", include_str!("../locales/de-DE.ftl"))],
        )
        .with_pinned_locale("de-DE");
        let ask = |device| catalog.t_args("device-consent-question", &device_args("Video", device));
        assert!(
            ask(tilo_capture::DeviceKind::Camera).ends_with("die Kamera verwenden?"),
            "{}",
            ask(tilo_capture::DeviceKind::Camera)
        );
        assert!(
            ask(tilo_capture::DeviceKind::Microphone).ends_with("das Mikrofon verwenden?"),
            "{}",
            ask(tilo_capture::DeviceKind::Microphone)
        );
    }
}
