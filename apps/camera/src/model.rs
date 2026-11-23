//! Pure data model for the Camera app.
//!
//! Everything here is plain data + pure logic — no rendering, no document,
//! no IO. The resource lives in [`crate::camera`]; the widget in
//! [`crate::app`]. Keeping the model pure makes it unit-testable without a
//! GPU or an event loop.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use camera_model::model::{CaptureKind, CodeKind, Facing, ScanEntry, ScannedCode, classify};

/// Top-level screen / mode the app is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Screen {
    Camera,
    Scanner,
    Document,
    Settings,
}

impl Screen {
    pub fn label(self) -> String {
        match self {
            Screen::Camera => crate::CATALOG.t("camera-screen-camera"),
            Screen::Scanner => crate::CATALOG.t("camera-screen-scanner"),
            Screen::Document => crate::CATALOG.t("camera-screen-document"),
            Screen::Settings => crate::CATALOG.t("camera-screen-settings"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Screen::Camera => "camera",
            Screen::Scanner => "scanner",
            Screen::Document => "document",
            Screen::Settings => "settings",
        }
    }
}

/// Photo vs. video capture sub-mode within the Camera screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureMode {
    Photo,
    Video,
}

impl CaptureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureMode::Photo => "photo",
            CaptureMode::Video => "video",
        }
    }
    /// The mode as the window names it.
    pub fn label(self) -> String {
        match self {
            CaptureMode::Photo => crate::CATALOG.t("camera-mode-photo"),
            CaptureMode::Video => crate::CATALOG.t("camera-mode-video"),
        }
    }
    pub fn parse_str(s: &str) -> CaptureMode {
        match s {
            "video" => CaptureMode::Video,
            _ => CaptureMode::Photo,
        }
    }
}

/// Capture resolution preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    R12MP,
    R4K,
    R1080p,
    R720p,
}

impl Resolution {
    pub const ALL: [Resolution; 4] = [
        Resolution::R12MP,
        Resolution::R4K,
        Resolution::R1080p,
        Resolution::R720p,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Resolution::R12MP => "12 MP",
            Resolution::R4K => "4K",
            Resolution::R1080p => "1080p",
            Resolution::R720p => "720p",
        }
    }
    /// Capture dimensions (w, h) for stored metadata.
    pub fn dims(self) -> (u32, u32) {
        match self {
            Resolution::R12MP => (4032, 3024),
            Resolution::R4K => (3840, 2160),
            Resolution::R1080p => (1920, 1080),
            Resolution::R720p => (1280, 720),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Resolution::R12MP => "12mp",
            Resolution::R4K => "4k",
            Resolution::R1080p => "1080p",
            Resolution::R720p => "720p",
        }
    }
    pub fn parse_str(s: &str) -> Resolution {
        match s {
            "4k" => Resolution::R4K,
            "1080p" => Resolution::R1080p,
            "720p" => Resolution::R720p,
            _ => Resolution::R12MP,
        }
    }
}

/// Self-timer delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Timer {
    Off,
    S3,
    S10,
}

impl Timer {
    pub const ALL: [Timer; 3] = [Timer::Off, Timer::S3, Timer::S10];
    pub fn label(self) -> String {
        match self {
            Timer::Off => crate::CATALOG.t("camera-timer-off"),
            Timer::S3 => "3 s".to_owned(),
            Timer::S10 => "10 s".to_owned(),
        }
    }
    pub fn seconds(self) -> i64 {
        match self {
            Timer::Off => 0,
            Timer::S3 => 3,
            Timer::S10 => 10,
        }
    }
    pub fn from_seconds(s: i64) -> Timer {
        match s {
            3 => Timer::S3,
            10 => Timer::S10,
            _ => Timer::Off,
        }
    }
}

/// Photo storage format. Limited to the two formats the app can genuinely
/// encode on disk — a real JPEG or a real PNG. (HEIF/RAW would need encoders
/// the app doesn't ship, so they're not offered rather than faked.)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhotoFormat {
    Jpeg,
    Png,
}

impl PhotoFormat {
    pub const ALL: [PhotoFormat; 2] = [PhotoFormat::Jpeg, PhotoFormat::Png];
    pub fn label(self) -> &'static str {
        match self {
            PhotoFormat::Jpeg => "JPEG",
            PhotoFormat::Png => "PNG",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            PhotoFormat::Jpeg => "jpeg",
            PhotoFormat::Png => "png",
        }
    }
    /// The file extension a photo in this format is saved under.
    pub fn extension(self) -> &'static str {
        match self {
            PhotoFormat::Jpeg => "jpg",
            PhotoFormat::Png => "png",
        }
    }
    pub fn parse_str(s: &str) -> PhotoFormat {
        match s {
            "png" => PhotoFormat::Png,
            _ => PhotoFormat::Jpeg,
        }
    }
}

/// User-tunable capture settings, as the settings screen shows them. They
/// are kept as two settings: [`CapturePreferences`] follow the person,
/// [`CameraDevicePreferences`] belong to this device's cameras.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent user-facing toggle shown flat on the settings screen"
)]
pub struct Settings {
    pub resolution: Resolution,
    pub photo_format: PhotoFormat,
    pub timer: Timer,
    pub grid: bool,
    pub level: bool,
    pub mirror_front: bool,
    pub geotag: bool,
    pub hdr: bool,
    pub default_facing: Facing,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            resolution: Resolution::R12MP,
            photo_format: PhotoFormat::Jpeg,
            timer: Timer::Off,
            grid: true,
            level: false,
            mirror_front: true,
            geotag: false,
            hdr: true,
            default_facing: Facing::Back,
        }
    }
}

impl Settings {
    /// The settings screen's values from the person's and this device's.
    pub fn from_parts(person: CapturePreferences, device: CameraDevicePreferences) -> Self {
        Self {
            resolution: device.resolution,
            photo_format: person.photo_format,
            timer: person.timer,
            grid: person.grid,
            level: person.level,
            mirror_front: person.mirror_front,
            geotag: person.geotag,
            hdr: person.hdr,
            default_facing: device.default_facing,
        }
    }

    /// The choices that follow the person.
    pub fn person(&self) -> CapturePreferences {
        CapturePreferences {
            photo_format: self.photo_format,
            timer: self.timer,
            grid: self.grid,
            level: self.level,
            mirror_front: self.mirror_front,
            geotag: self.geotag,
            hdr: self.hdr,
        }
    }

    /// The choices about this device's cameras.
    pub fn device(&self) -> CameraDevicePreferences {
        CameraDevicePreferences {
            resolution: self.resolution,
            default_facing: self.default_facing,
        }
    }
}

/// How the person likes to take pictures: the same on each of their
/// devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent user-facing toggle"
)]
pub struct CapturePreferences {
    pub photo_format: PhotoFormat,
    pub timer: Timer,
    pub grid: bool,
    pub level: bool,
    pub mirror_front: bool,
    pub geotag: bool,
    pub hdr: bool,
}

impl Default for CapturePreferences {
    fn default() -> Self {
        Settings::default().person()
    }
}

impl settings_model::SettingsSchema for CapturePreferences {
    const SCOPE: settings_model::Scope = settings_model::Scope::Personal;
}

/// Choices about this device's own cameras: which resolutions and lenses
/// exist differs from one device to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraDevicePreferences {
    pub resolution: Resolution,
    pub default_facing: Facing,
}

impl Default for CameraDevicePreferences {
    fn default() -> Self {
        Settings::default().device()
    }
}

impl settings_model::SettingsSchema for CameraDevicePreferences {
    const SCOPE: settings_model::Scope = settings_model::Scope::DeviceLocal;
}

/// Camera's single recent capture handoff.
///
/// A capture may have a local projection (`path`) and/or a verified portable
/// media identity (`media_id`). The latter names bytes in the content store;
/// peers never receive a filesystem path from the document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    /// Stable identity — the file name for disk-backed items.
    pub id: String,
    pub kind: CaptureKind,
    /// Unix epoch seconds (file mtime for disk-backed items).
    pub ts: i64,
    pub w: u32,
    pub h: u32,
    /// Video duration in milliseconds (0 for photos / unknown).
    pub dur_ms: i64,
    pub label: String,
    pub facing: Facing,
    /// Procedural scene seed for the sample/hermetic path only.
    pub scene: u8,
    /// Absolute path to the backing file, when disk-backed.
    pub path: Option<PathBuf>,
    /// `media:blake3:<hash>` for shared capture bytes, if published.
    pub media_id: Option<String>,
}

impl Capture {
    pub fn is_video(&self) -> bool {
        matches!(self.kind, CaptureKind::Video)
    }
}

/// How the window names the lens `facing`.
pub fn facing_label(facing: Facing) -> String {
    let message = match facing {
        // The rear lens rather than "back", so the lens selector never reads as a
        // navigational back button.
        Facing::Back => "camera-facing-rear",
        Facing::Front => "camera-facing-front",
        Facing::Wide => "camera-facing-wide",
    };
    crate::CATALOG.t(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_and_facing_round_trip_and_cycle() {
        {
            let mut f = Facing::Back;
            f = f.next();
            assert_eq!(f, Facing::Front);
            f = f.next();
            assert_eq!(f, Facing::Wide);
            f = f.next();
            assert_eq!(f, Facing::Back);
        }
    }

    #[test]
    fn code_classification_parses_payloads_and_matches_actions() {
        {
            assert_eq!(classify("https://example.test/app"), CodeKind::Url);
            assert_eq!(classify("HTTP://X"), CodeKind::Url);
        }
        {
            let k = classify("WIFI:T:WPA2;S:Tilo Guest;P:hunter2;H:false;;");
            match k {
                CodeKind::Wifi {
                    ssid,
                    auth,
                    password,
                    hidden,
                } => {
                    assert_eq!(ssid, "Tilo Guest");
                    assert_eq!(auth, "WPA2");
                    assert_eq!(password, "hunter2");
                    assert!(!hidden);
                }
                other => panic!("expected wifi, got {other:?}"),
            }
        }
        {
            let v = "BEGIN:VCARD\nVERSION:3.0\nFN:Ada Lovelace\nTEL:+1-555-0100\nEMAIL:ada@analytical.engine\nORG:Analytical Engines\nEND:VCARD";
            match classify(v) {
                CodeKind::Contact {
                    name,
                    phone,
                    email,
                    org,
                } => {
                    assert_eq!(name, "Ada Lovelace");
                    assert_eq!(phone, "+1-555-0100");
                    assert_eq!(email, "ada@analytical.engine");
                    assert_eq!(org, "Analytical Engines");
                }
                other => panic!("expected contact, got {other:?}"),
            }
        }
        {
            assert_eq!(classify("https://x").actions()[0], "Open link");
            assert_eq!(classify("WIFI:S:x;;").actions()[0], "Copy password");
            assert_eq!(classify("tel:+15550100").actions()[0], "Call");
        }
    }
}
