//! The desktop appearance in the vocabularies legacy toolkits already read.
//!
//! Tilo's own apps take their look from `Theme`. GTK, Qt, Electron and Firefox do not: a
//! sandboxed one asks the Settings portal for `org.freedesktop.appearance` and
//! `org.gnome.desktop.interface`, an unsandboxed one reads the same GNOME keys from dconf.
//! [`LegacyAppearance`] is the one projection of `settings.json` both paths publish, so a
//! theme change in Settings reaches every toolkit with identical values.

use super::desktop_services::PortalSetting;

pub const APPEARANCE: &str = "org.freedesktop.appearance";
pub const GNOME_INTERFACE: &str = "org.gnome.desktop.interface";
pub const GNOME_A11Y_INTERFACE: &str = "org.gnome.desktop.a11y.interface";

/// Point size GNOME uses for its interface fonts; text scaling is published separately as
/// `text-scaling-factor`, so the base stays fixed.
const INTERFACE_FONT_POINTS: u32 = 11;
/// Adwaita ships with every GTK install and is what GNOME applications expect by default.
const CURSOR_THEME: &str = "Adwaita";

/// GNOME's named accent colours with the sRGB values libadwaita renders for them.
const GNOME_ACCENTS: [(&str, [u8; 3]); 9] = [
    ("blue", [0x35, 0x84, 0xe4]),
    ("teal", [0x21, 0x90, 0xa4]),
    ("green", [0x3a, 0x94, 0x4a]),
    ("yellow", [0xc8, 0x88, 0x00]),
    ("orange", [0xed, 0x5b, 0x00]),
    ("red", [0xe6, 0x2d, 0x42]),
    ("pink", [0xd5, 0x61, 0x99]),
    ("purple", [0x91, 0x41, 0xac]),
    ("slate", [0x6f, 0x83, 0x96]),
];

#[derive(Clone, Debug, PartialEq)]
pub struct LegacyAppearance {
    pub dark: bool,
    pub high_contrast: bool,
    pub reduce_motion: bool,
    pub accent: [u8; 3],
    pub sans_family: String,
    pub mono_family: String,
    pub text_scale: f64,
    pub cursor_size: i32,
}

impl LegacyAppearance {
    /// The appearance the configured settings file describes, including a session-wide
    /// reduced-motion request from the host environment.
    pub fn configured() -> Self {
        let text = settings_store::settings_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        Self::from_settings_text(&text, ui::host::environment().accessibility.reduce_motion)
    }

    /// Pure projection of a `settings.json` body. A missing or malformed document yields the
    /// dark default the shell itself falls back to.
    pub fn from_settings_text(text: &str, reduce_motion_override: bool) -> Self {
        let document = settings_store::parse(text);
        let theme = ui::Theme::resolved_from_settings(text);
        let high_contrast = theme.colors_are_forced();
        let background = theme.tokens.colors.surfaces.content.to_rgba8();
        let luma = 0.2126 * f64::from(background.r)
            + 0.7152 * f64::from(background.g)
            + 0.0722 * f64::from(background.b);
        let accent = theme.tokens.colors.intent.inverted.to_rgba8();
        let accessibility = settings_store::accessibility::AccessibilitySettings::from_section(
            &settings_store::section(text, settings_store::accessibility::SECTION),
        );
        Self {
            dark: luma < 127.5,
            high_contrast,
            reduce_motion: reduce_motion_override
                || theme.reduce_motion
                || settings_store::bool_at(&document, "appearance", "reduce_motion")
                    .unwrap_or(false),
            accent: [accent.r, accent.g, accent.b],
            sans_family: theme.tokens.typography.sans_family.clone(),
            mono_family: theme.tokens.typography.mono_family.clone(),
            text_scale: settings_store::f64_in(
                &document,
                "appearance",
                "text_scale",
                0.5..=4.0,
                1.0,
            ),
            cursor_size: i32::try_from(accessibility.effective_cursor_size()).unwrap_or(i32::MAX),
        }
    }

    fn gnome_color_scheme(&self) -> &'static str {
        if self.dark {
            "prefer-dark"
        } else {
            "prefer-light"
        }
    }

    fn gtk_theme(&self) -> &'static str {
        if self.dark { "Adwaita-dark" } else { "Adwaita" }
    }

    /// The nearest GNOME named accent, since GNOME's key is an enumeration rather than a colour.
    pub fn gnome_accent(&self) -> &'static str {
        let distance = |named: [u8; 3]| -> u32 {
            named
                .iter()
                .zip(self.accent)
                .map(|(&a, b)| u32::from(a.abs_diff(b)).pow(2))
                .sum()
        };
        GNOME_ACCENTS
            .iter()
            .min_by_key(|(_, rgb)| distance(*rgb))
            .map_or("blue", |(name, _)| name)
    }

    fn font(family: &str) -> String {
        format!("{family} {INTERFACE_FONT_POINTS}")
    }

    /// Every key the Settings portal serves, as `(namespace, key, value)`.
    pub fn portal_settings(&self) -> Vec<(&'static str, &'static str, PortalSetting)> {
        let [r, g, b] = self.accent.map(|channel| f64::from(channel) / 255.0);
        vec![
            (
                APPEARANCE,
                "color-scheme",
                PortalSetting::Unsigned(if self.dark { 1 } else { 2 }),
            ),
            (
                APPEARANCE,
                "contrast",
                PortalSetting::Unsigned(u32::from(self.high_contrast)),
            ),
            (APPEARANCE, "accent-color", PortalSetting::Color(r, g, b)),
            (
                APPEARANCE,
                "reduced-motion",
                PortalSetting::Unsigned(u32::from(self.reduce_motion)),
            ),
            (
                GNOME_INTERFACE,
                "color-scheme",
                PortalSetting::Text(self.gnome_color_scheme().into()),
            ),
            (
                GNOME_INTERFACE,
                "gtk-theme",
                PortalSetting::Text(self.gtk_theme().into()),
            ),
            (
                GNOME_INTERFACE,
                "accent-color",
                PortalSetting::Text(self.gnome_accent().into()),
            ),
            (
                GNOME_INTERFACE,
                "font-name",
                PortalSetting::Text(Self::font(&self.sans_family)),
            ),
            (
                GNOME_INTERFACE,
                "monospace-font-name",
                PortalSetting::Text(Self::font(&self.mono_family)),
            ),
            (
                GNOME_INTERFACE,
                "text-scaling-factor",
                PortalSetting::Double(self.text_scale),
            ),
            (
                GNOME_INTERFACE,
                "cursor-theme",
                PortalSetting::Text(CURSOR_THEME.into()),
            ),
            (
                GNOME_INTERFACE,
                "cursor-size",
                PortalSetting::Int(self.cursor_size),
            ),
            (
                GNOME_INTERFACE,
                "enable-animations",
                PortalSetting::Bool(!self.reduce_motion),
            ),
            (
                GNOME_A11Y_INTERFACE,
                "high-contrast",
                PortalSetting::Bool(self.high_contrast),
            ),
        ]
    }

    /// The GNOME keys as a `dconf load /` keyfile, for host applications that read GSettings
    /// directly. Only the GSettings-backed namespaces are written; `org.freedesktop.appearance`
    /// exists only on the portal.
    pub fn dconf_keyfile(&self) -> String {
        let mut sections: Vec<(&str, Vec<String>)> = Vec::new();
        for (namespace, key, value) in self.portal_settings() {
            if namespace == APPEARANCE {
                continue;
            }
            let Some(literal) = gvariant_literal(&value) else {
                continue;
            };
            let path = namespace.replace('.', "/");
            let line = format!("{key}={literal}");
            match sections.iter_mut().find(|(name, _)| *name == namespace) {
                Some((_, lines)) => lines.push(line),
                None => {
                    sections.push((namespace, vec![format!("[{path}]"), line]));
                }
            }
        }
        let mut keyfile = String::new();
        for (_, lines) in sections {
            for line in lines {
                keyfile.push_str(&line);
                keyfile.push('\n');
            }
            keyfile.push('\n');
        }
        keyfile
    }
}

/// GVariant text format for the value types GSettings keys use here. Doubles always carry a
/// decimal point, or dconf would store an `int32` that GSettings then rejects for a `d` key.
fn gvariant_literal(value: &PortalSetting) -> Option<String> {
    Some(match value {
        PortalSetting::Text(text) => {
            let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
            format!("'{escaped}'")
        }
        PortalSetting::Bool(flag) => flag.to_string(),
        PortalSetting::Int(number) => number.to_string(),
        PortalSetting::Unsigned(number) => format!("uint32 {number}"),
        PortalSetting::Double(number) => format!("{number:?}"),
        PortalSetting::Color(..) => return None,
    })
}

/// Hand the GNOME keys to dconf so unsandboxed host applications follow the OS appearance.
/// Skips the write when nothing changed since the last successful one.
#[cfg(target_os = "linux")]
pub struct DconfPublisher {
    last_written: Option<String>,
}

#[cfg(target_os = "linux")]
impl DconfPublisher {
    pub const fn new() -> Self {
        Self { last_written: None }
    }

    pub async fn publish(&mut self, appearance: &LegacyAppearance) {
        use tokio::io::AsyncWriteExt as _;
        let keyfile = appearance.dconf_keyfile();
        if self.last_written.as_ref() == Some(&keyfile) {
            return;
        }
        let child = tokio::process::Command::new("dconf")
            .args(["load", "/"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(%error, "dconf is unavailable; host GTK apps keep their own appearance");
                return;
            }
        };
        if let Some(mut stdin) = child.stdin.take()
            && let Err(error) = stdin.write_all(keyfile.as_bytes()).await
        {
            tracing::warn!(%error, "could not hand the appearance to dconf");
            return;
        }
        match child.wait_with_output().await {
            Ok(output) if output.status.success() => self.last_written = Some(keyfile),
            Ok(output) => tracing::warn!(
                status = %output.status,
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "dconf refused the appearance keys"
            ),
            Err(error) => tracing::warn!(%error, "dconf did not finish"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "values are copied, not computed")]
mod tests {
    use super::*;

    fn lookup<'a>(
        settings: &'a [(&'static str, &'static str, PortalSetting)],
        namespace: &str,
        key: &str,
    ) -> &'a PortalSetting {
        &settings
            .iter()
            .find(|(n, k, _)| *n == namespace && *k == key)
            .unwrap_or_else(|| panic!("{namespace}/{key} is published"))
            .2
    }

    #[test]
    fn a_missing_settings_file_publishes_the_dark_default_everywhere() {
        let appearance = LegacyAppearance::from_settings_text("", false);
        let settings = appearance.portal_settings();
        assert_eq!(
            lookup(&settings, APPEARANCE, "color-scheme"),
            &PortalSetting::Unsigned(1)
        );
        assert_eq!(
            lookup(&settings, APPEARANCE, "reduced-motion"),
            &PortalSetting::Unsigned(0)
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "color-scheme"),
            &PortalSetting::Text("prefer-dark".into())
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "gtk-theme"),
            &PortalSetting::Text("Adwaita-dark".into())
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "font-name"),
            &PortalSetting::Text(format!("{} 11", ui::text_render::UI_SANS_FAMILY))
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "monospace-font-name"),
            &PortalSetting::Text(format!("{} 11", ui::text_render::UI_MONOSPACE_FAMILY))
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "text-scaling-factor"),
            &PortalSetting::Double(1.0)
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "cursor-theme"),
            &PortalSetting::Text("Adwaita".into())
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "cursor-size"),
            &PortalSetting::Int(24)
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "enable-animations"),
            &PortalSetting::Bool(true)
        );
    }

    #[test]
    fn a_light_theme_with_accessibility_choices_reaches_every_namespace() {
        let text = r#"{
            "appearance": {"mode": "light", "reduce_motion": true, "text_scale": 1.25},
            "accessibility": {"large_cursor": true, "cursor_size": 48}
        }"#;
        let appearance = LegacyAppearance::from_settings_text(text, false);
        let settings = appearance.portal_settings();
        assert_eq!(
            lookup(&settings, APPEARANCE, "color-scheme"),
            &PortalSetting::Unsigned(2)
        );
        assert_eq!(
            lookup(&settings, APPEARANCE, "reduced-motion"),
            &PortalSetting::Unsigned(1)
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "color-scheme"),
            &PortalSetting::Text("prefer-light".into())
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "gtk-theme"),
            &PortalSetting::Text("Adwaita".into())
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "enable-animations"),
            &PortalSetting::Bool(false)
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "text-scaling-factor"),
            &PortalSetting::Double(1.25)
        );
        assert_eq!(
            lookup(&settings, GNOME_INTERFACE, "cursor-size"),
            &PortalSetting::Int(48)
        );
    }

    #[test]
    fn high_contrast_is_honoured_as_a_flag_and_as_a_theme_and_keeps_its_polarity() {
        for (text, scheme) in [
            (
                r#"{"appearance": {"mode": "light", "high_contrast": true}}"#,
                2,
            ),
            (r#"{"appearance": {"dark_theme": "high-contrast"}}"#, 1),
        ] {
            let settings = LegacyAppearance::from_settings_text(text, false).portal_settings();
            assert_eq!(
                lookup(&settings, APPEARANCE, "contrast"),
                &PortalSetting::Unsigned(1),
                "{text}"
            );
            assert_eq!(
                lookup(&settings, GNOME_A11Y_INTERFACE, "high-contrast"),
                &PortalSetting::Bool(true),
                "{text}"
            );
            assert_eq!(
                lookup(&settings, APPEARANCE, "color-scheme"),
                &PortalSetting::Unsigned(scheme),
                "high contrast keeps the mode's polarity: {text}"
            );
        }
    }

    #[test]
    fn the_session_reduced_motion_request_wins_over_the_file() {
        let appearance = LegacyAppearance::from_settings_text("{}", true);
        assert!(appearance.reduce_motion);
    }

    #[test]
    fn accent_maps_to_the_nearest_gnome_name() {
        let mut appearance = LegacyAppearance::from_settings_text("", false);
        appearance.accent = [0xe0, 0x30, 0x40];
        assert_eq!(appearance.gnome_accent(), "red");
        appearance.accent = [0x30, 0x80, 0xe0];
        assert_eq!(appearance.gnome_accent(), "blue");
    }

    #[test]
    fn dconf_keyfile_carries_only_gsettings_keys_in_typed_text_form() {
        let mut appearance = LegacyAppearance::from_settings_text("", false);
        appearance.sans_family = "O'Brien Sans".into();
        let keyfile = appearance.dconf_keyfile();
        assert!(keyfile.starts_with("[org/gnome/desktop/interface]\n"));
        assert!(keyfile.contains("\n[org/gnome/desktop/a11y/interface]\nhigh-contrast=false\n"));
        assert!(keyfile.contains("\ncolor-scheme='prefer-dark'\n"));
        assert!(keyfile.contains("\nfont-name='O\\'Brien Sans 11'\n"));
        assert!(keyfile.contains("\ntext-scaling-factor=1.0\n"));
        assert!(keyfile.contains("\ncursor-size=24\n"));
        assert!(keyfile.contains("\nenable-animations=true\n"));
        assert!(
            !keyfile.contains("reduced-motion") && !keyfile.contains("contrast=uint32"),
            "org.freedesktop.appearance is portal-only: {keyfile}"
        );
    }
}
