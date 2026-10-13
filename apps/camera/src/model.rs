//! Pure data model for the Camera app.
//!
//! Everything here is plain data + pure logic — no rendering, no document,
//! no IO. The record lives in [`crate::record`]; the widget in
//! [`crate::app`]. Keeping the model pure makes it unit-testable without a
//! GPU or an event loop.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema, Model};

/// Top-level screen / mode the app is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Screen {
    Camera,
    Scanner,
    Document,
    Settings,
}

impl Screen {
    pub fn label(self) -> &'static str {
        match self {
            Screen::Camera => "Camera",
            Screen::Scanner => "Scan",
            Screen::Document => "Documents",
            Screen::Settings => "Settings",
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
    pub fn parse_str(s: &str) -> CaptureMode {
        match s {
            "video" => CaptureMode::Video,
            _ => CaptureMode::Photo,
        }
    }
}

/// Which physical camera is selected.
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Facing {
    #[default]
    Back,
    Front,
    Wide,
}

impl Facing {
    pub const ALL: [Facing; 3] = [Facing::Back, Facing::Front, Facing::Wide];
    pub fn label(self) -> &'static str {
        match self {
            // "Rear" rather than "Back" so the lens selector never reads as a
            // navigational back button.
            Facing::Back => "Rear",
            Facing::Front => "Front",
            Facing::Wide => "Ultra-wide",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Facing::Back => "back",
            Facing::Front => "front",
            Facing::Wide => "wide",
        }
    }
    pub fn parse_str(s: &str) -> Facing {
        match s {
            "front" => Facing::Front,
            "wide" => Facing::Wide,
            _ => Facing::Back,
        }
    }
    #[must_use]
    pub fn next(self) -> Facing {
        match self {
            Facing::Back => Facing::Front,
            Facing::Front => Facing::Wide,
            Facing::Wide => Facing::Back,
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
    pub fn label(self) -> &'static str {
        match self {
            Timer::Off => "Off",
            Timer::S3 => "3s",
            Timer::S10 => "10s",
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

impl ui::storage::SettingsSchema for CapturePreferences {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Personal;
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

impl ui::storage::SettingsSchema for CameraDevicePreferences {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::DeviceLocal;
}

#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CaptureKind {
    #[default]
    Photo,
    Video,
}

impl CaptureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureKind::Photo => "photo",
            CaptureKind::Video => "video",
        }
    }
    pub fn parse_str(s: &str) -> CaptureKind {
        match s {
            "video" => CaptureKind::Video,
            _ => CaptureKind::Photo,
        }
    }
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

/// One entry in the scan history — the raw decoded text plus when it was
/// seen. The [`CodeKind`] and display summary are derived on demand via
/// [`classify`], so only the ground-truth payload is persisted.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanEntry {
    /// What the code said.
    pub raw: String,
    /// When it was scanned, in Unix seconds.
    pub ts: i64,
}

/// Classified payload of a decoded code, used to surface contextual
/// actions in the scanner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeKind {
    Url,
    Wifi {
        ssid: String,
        auth: String,
        password: String,
        hidden: bool,
    },
    Contact {
        name: String,
        phone: String,
        email: String,
        org: String,
    },
    Email,
    Phone,
    Geo,
    Text,
}

impl CodeKind {
    /// Human-readable kind for the scanner header.
    pub fn title(&self) -> &'static str {
        match self {
            CodeKind::Url => "Website",
            CodeKind::Wifi { .. } => "Wi-Fi network",
            CodeKind::Contact { .. } => "Contact card",
            CodeKind::Email => "Email address",
            CodeKind::Phone => "Phone number",
            CodeKind::Geo => "Location",
            CodeKind::Text => "Plain text",
        }
    }

    /// The actions offered for this payload, primary first. Each names
    /// exactly what it does.
    pub fn actions(&self) -> Vec<&'static str> {
        match self {
            CodeKind::Url => vec!["Open link", "Copy link"],
            CodeKind::Wifi { .. } => vec!["Copy password", "Copy network name"],
            CodeKind::Contact { phone, .. } if !phone.is_empty() => {
                vec!["Call", "Copy contact"]
            }
            CodeKind::Contact { .. } => vec!["Copy contact"],
            CodeKind::Email => vec!["Write email", "Copy address"],
            CodeKind::Phone => vec!["Call", "Copy number"],
            CodeKind::Geo => vec!["Open map", "Copy location"],
            CodeKind::Text => vec!["Copy text"],
        }
    }

    /// The payload's one main fact: a network name, a person, an address.
    pub fn summary(&self, raw: &str) -> String {
        let without = |scheme: &str| {
            raw.get(..scheme.len())
                .filter(|head| head.eq_ignore_ascii_case(scheme))
                .and_then(|_| raw.get(scheme.len()..))
                .unwrap_or(raw)
                .to_owned()
        };
        match self {
            CodeKind::Wifi { ssid, .. } => ssid.clone(),
            CodeKind::Contact { name, .. } => name.clone(),
            CodeKind::Email => without("mailto:"),
            CodeKind::Phone => without("tel:"),
            CodeKind::Geo => without("geo:").replace(',', ", "),
            CodeKind::Url | CodeKind::Text => raw.to_string(),
        }
    }

    /// What kind of payload it is, with the one detail that tells two of a
    /// kind apart.
    pub fn description(&self) -> String {
        match self {
            CodeKind::Wifi { auth, .. } if !auth.is_empty() && auth != "nopass" => {
                format!("Wi-Fi network, {auth}")
            }
            CodeKind::Contact { phone, .. } if !phone.is_empty() => {
                format!("Contact, {phone}")
            }
            other => other.title().to_owned(),
        }
    }
}

/// A code detected in the viewfinder: its raw text, classification, the
/// barcode-symbology label, and the four corner points (in frame pixel
/// coordinates) returned by the real decode.
#[derive(Clone, Debug, PartialEq)]
pub struct ScannedCode {
    pub raw: String,
    pub kind: CodeKind,
    pub symbology: String,
    /// Four corners in frame pixel space, clockwise from top-left.
    pub corners: [(f32, f32); 4],
}

impl ScannedCode {}

/// Classify a raw decoded string into a [`CodeKind`].
pub fn classify(raw: &str) -> CodeKind {
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        CodeKind::Url
    } else if lower.starts_with("wifi:") {
        let (ssid, auth, password, hidden) = parse_wifi(raw);
        CodeKind::Wifi {
            ssid,
            auth,
            password,
            hidden,
        }
    } else if lower.starts_with("begin:vcard") || lower.starts_with("mecard:") {
        let (name, phone, email, org) = parse_contact(raw);
        CodeKind::Contact {
            name,
            phone,
            email,
            org,
        }
    } else if lower.starts_with("mailto:") {
        CodeKind::Email
    } else if lower.starts_with("tel:") {
        CodeKind::Phone
    } else if lower.starts_with("geo:") {
        CodeKind::Geo
    } else {
        CodeKind::Text
    }
}

/// Parse a `WIFI:T:WPA;S:SSID;P:pass;H:false;;` payload.
fn parse_wifi(raw: &str) -> (String, String, String, bool) {
    let body = raw
        .strip_prefix("WIFI:")
        .or_else(|| raw.strip_prefix("wifi:"))
        .unwrap_or(raw);
    let mut ssid = String::new();
    let mut auth = String::from("Open");
    let mut password = String::new();
    let mut hidden = false;
    for field in body.split(';') {
        if let Some((k, v)) = field.split_once(':') {
            match k.to_ascii_uppercase().as_str() {
                "S" => ssid = v.to_string(),
                "T" => {
                    auth = if v.is_empty() {
                        "Open".into()
                    } else {
                        v.to_string()
                    }
                }
                "P" => password = v.to_string(),
                "H" => hidden = v.eq_ignore_ascii_case("true"),
                _ => {}
            }
        }
    }
    (ssid, auth, password, hidden)
}

/// Parse the handful of vCard / MECARD fields we surface.
fn parse_contact(raw: &str) -> (String, String, String, String) {
    let mut name = String::new();
    let mut phone = String::new();
    let mut email = String::new();
    let mut org = String::new();
    // vCard: line-oriented `FN:`, `TEL:`, `EMAIL:`, `ORG:`.
    for line in raw.lines() {
        let line = line.trim();
        if let Some(v) = field_after(line, "FN") {
            name = v;
        } else if let Some(v) = field_after(line, "TEL") {
            if phone.is_empty() {
                phone = v;
            }
        } else if let Some(v) = field_after(line, "EMAIL") {
            email = v;
        } else if let Some(v) = field_after(line, "ORG") {
            org = v;
        } else if let Some(v) = field_after(line, "N")
            && name.is_empty()
        {
            name = v.replace(';', " ").trim().to_string();
        }
    }
    // MECARD: `MECARD:N:Name;TEL:123;EMAIL:a@b;;`
    if name.is_empty() {
        for field in raw.split(';') {
            if let Some(v) = field
                .strip_prefix("MECARD:N:")
                .or_else(|| field.strip_prefix("N:"))
            {
                name = v.to_string();
            }
        }
    }
    (name, phone, email, org)
}

/// Match a vCard property line `KEY[;params]:value`, returning `value`.
fn field_after(line: &str, key: &str) -> Option<String> {
    let (head, value) = line.split_once(':')?;
    let prop = head.split(';').next().unwrap_or(head);
    if prop.eq_ignore_ascii_case(key) {
        Some(value.trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_and_facing_round_trip_and_cycle() {
        // Phase `facing_cycles_through_all`.
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
        // Phase `classify_url`.
        {
            assert_eq!(classify("https://example.test/app"), CodeKind::Url);
            assert_eq!(classify("HTTP://X"), CodeKind::Url);
        }
        // Phase `classify_and_parse_wifi`.
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
        // Phase `classify_and_parse_contact_vcard`.
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
        // Phase `actions_match_kind`.
        {
            assert_eq!(classify("https://x").actions()[0], "Open link");
            assert_eq!(classify("WIFI:S:x;;").actions()[0], "Copy password");
            assert_eq!(classify("tel:+15550100").actions()[0], "Call");
        }
    }
}
