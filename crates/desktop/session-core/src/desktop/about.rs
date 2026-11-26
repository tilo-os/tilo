//! What this Tilo is, said the same way wherever Tilo says it: [`AboutVariant`] picks the words for
//! where Tilo runs, [`AboutLinks`] is the deployment's set of links (one left empty is not offered),
//! and [`about_copy`] the words a shell's About sheet shows.

use crate::desktop::i18n::{t, t_args};

/// Tilo's home on the web: where to read about it and get it.
pub const TILO_HOME: &str = "https://tilo.so";

/// Where the Tilo being described runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AboutVariant {
    /// Installed on this computer.
    Native,
    /// In a browser tab, ahead of running on computers natively.
    Browser,
}

/// The deployment's links. Each is an `http(s)` URL or absent; anything else (an empty setting
/// included) is not offered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AboutLinks {
    /// Where to be told when Tilo comes to computers.
    pub notify: Option<String>,
    /// Tilo's source code.
    pub source: Option<String>,
    /// Where the people who use and build Tilo talk.
    pub community: Option<String>,
}

impl AboutLinks {
    /// The links from their settings; an empty or non-web setting is none.
    pub fn new(notify: &str, source: &str, community: &str) -> Self {
        fn web(url: &str) -> Option<String> {
            let url = url.trim();
            (url.starts_with("https://") || url.starts_with("http://"))
                .then(|| url.to_owned())
                .filter(|url| !url.contains(char::is_whitespace))
        }
        Self {
            notify: web(notify),
            source: web(source),
            community: web(community),
        }
    }

    /// The links offered, in reading order, as `(label, url)` .
    pub fn shown(&self) -> Vec<(String, String)> {
        [
            ("tilo-about-link-notify", &self.notify),
            ("tilo-about-link-source", &self.source),
            ("tilo-about-link-community", &self.community),
        ]
        .into_iter()
        .filter_map(|(key, url)| url.as_ref().map(|url| (t(key), url.clone())))
        .collect()
    }
}

/// Read from a host's settings as `{notify, source, community}` , each a URL or empty; a missing
/// one is empty.
impl<'de> serde::Deserialize<'de> for AboutLinks {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Settings {
            notify: String,
            source: String,
            community: String,
        }
        let settings = Settings::deserialize(deserializer)?;
        Ok(Self::new(
            &settings.notify,
            &settings.source,
            &settings.community,
        ))
    }
}

/// The About sheet's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AboutCopy {
    /// The sheet's title.
    pub title: String,
    /// What leads the sheet: this computer's name, or in a browser that this is Tilo.
    pub heading: Option<String>,
    /// In a browser, where Tilo runs.
    pub body: Option<String>,
    /// Which Tilo this is.
    pub version: String,
    /// Tilo's home on the web ([`TILO_HOME`]): in a browser, where to get Tilo; on a computer that
    /// has it, just its address.
    pub home: String,
}

/// The words for Tilo running as `variant` . A browser tab is not "this computer": there it is
/// About Tilo.
pub fn about_copy(variant: AboutVariant) -> AboutCopy {
    let version = t_args(
        "tilo-about-version",
        &[("version", env!("CARGO_PKG_VERSION").into())],
    );
    match variant {
        AboutVariant::Native => AboutCopy {
            title: t("tilo-about-title"),
            heading: device_name(),
            body: None,
            version,
            home: t("tilo-about-link-home"),
        },
        AboutVariant::Browser => AboutCopy {
            title: t("tilo-about-title-browser"),
            heading: Some(t("tilo-about-lead")),
            body: Some(t("tilo-about-body-browser")),
            version,
            home: t("tilo-about-link-get"),
        },
    }
}

/// This computer's name, as its host name gives it; none when it has no host name.
#[must_use]
pub fn device_name() -> Option<String> {
    tilo_failure::ignored!(
        "a computer without a host name is still this computer, left unnamed",
        std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::i18n::t;

    tilo_test_env::scenario! {
        fn the_about_sheet_says_where_this_tilo_runs;

        phase empty_or_non_web_links_are_not_offered;
        {
            tilo_ui_base::i18n::I18nManager::init("en-US");
            let links = AboutLinks::new(" https://example.org/notify ", "", "javascript:alert(1)");
            assert_eq!(links.notify.as_deref(), Some("https://example.org/notify"));
            assert_eq!(links.source, None);
            assert_eq!(links.community, None);
            assert_eq!(
                links.shown(),
                vec![(
                    t("tilo-about-link-notify"),
                    "https://example.org/notify".to_owned()
                )]
            );
            assert!(AboutLinks::new("", "", "").shown().is_empty());
        }

        phase the_browser_says_it_runs_in_the_browser;
        {
            tilo_ui_base::i18n::I18nManager::init("en-US");
            let browser = about_copy(AboutVariant::Browser);
            let native = about_copy(AboutVariant::Native);
            let body = browser.body.clone().unwrap_or_default();
            assert!(body.contains("browser"), "{body}");
            assert!(native.body.is_none(), "a computer's sheet says nothing of where it runs");
            assert_eq!(native.heading, device_name(), "a computer's sheet leads with its name");
            // A browser tab is not "this computer".
            for words in [&browser.title, &body] {
                assert!(
                    !words.contains("computer") || words.contains("own computer"),
                    "{words}"
                );
            }
            assert!(browser.title.contains("Tilo"), "{}", browser.title);
            assert!(native.title.contains("computer"), "{}", native.title);
            // Both say which Tilo this is, and where to find it on the web; the browser says that is
            // where to get it.
            for copy in [&browser, &native] {
                assert!(copy.version.starts_with("Tilo "), "{}", copy.version);
                assert!(
                    copy.version.contains(env!("CARGO_PKG_VERSION")),
                    "{}",
                    copy.version
                );
            }
            assert!(browser.home.contains("Get"), "{}", browser.home);
            assert_eq!(native.home, "tilo.so");
        }
    }

    #[test]
    fn settings_with_empty_or_missing_links_offer_only_the_rest() {
        let links: AboutLinks =
            serde_json::from_str(r#"{"notify":"","source":"https://example.org/src"}"#)
                .expect("about settings");
        assert_eq!(
            links,
            AboutLinks {
                notify: None,
                source: Some("https://example.org/src".to_owned()),
                community: None,
            }
        );
        let none: AboutLinks = serde_json::from_str("{}").expect("no about settings");
        assert_eq!(none, AboutLinks::default());
    }
}
