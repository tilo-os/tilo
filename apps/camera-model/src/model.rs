//! The values Camera keeps and the pure logic over them: the lens, the kind of capture, the
//! scans and what a decoded code is. No rendering, no document, no IO.

use serde::{Deserialize, Serialize};
use tilo_resource::{self, JsonSchema, Model};

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

/// Parse the vCard and MECARD fields this app reads.
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
