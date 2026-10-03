//! The runtime's Fluent catalogue: the words it shows or exports itself
//! (the device consent prompt and the device grants), resolved in the
//! person's locale, `en-US` where a translation lacks a message.

use ui::i18n::{ArgValue, EmbeddedCatalog};

static CATALOG: EmbeddedCatalog = catalog();

const fn catalog() -> EmbeddedCatalog {
    let catalog = EmbeddedCatalog::new(
        "en-US",
        &[
            ("en-US", include_str!("../locales/en-US.ftl")),
            ("de-DE", include_str!("../locales/de-DE.ftl")),
        ],
        "device-consent-question",
    )
    .with_system_locale();
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
}

pub(crate) fn t(key: &str) -> String {
    CATALOG.t(key)
}

pub(crate) fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    CATALOG.t_args(key, args)
}

/// The arguments of a message about `app` and `device`: each catalogue
/// selects its own sentence on the device, since a spliced-in noun does not
/// agree with the words around it in every language.
pub(crate) fn device_args(
    app: &str,
    device: tilo_capture::DeviceKind,
) -> [(&'static str, ArgValue); 2] {
    [("app", app.into()), ("device", device.name().into())]
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn german_names_each_device_with_its_own_article() {
        let catalog = EmbeddedCatalog::new(
            "de-DE",
            &[("de-DE", include_str!("../locales/de-DE.ftl"))],
            "device-consent-question",
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
