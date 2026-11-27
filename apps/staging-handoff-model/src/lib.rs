//! The staging ESP handoff: what Tilo Imager leaves at `/EFI/tilo-staging/handoff.json` for
//! the live installer it staged beside the retained OS, and the EFI boot records around it.

use serde::{Deserialize, Serialize};

pub mod efi;
pub mod gpt;
pub mod sizes;

pub const HANDOFF_FILE: &str = "handoff.json";
pub const SCHEMA: u32 = 1;
pub const MAX_BYTES: usize = 64 * 1024;
const MAX_LBA: u64 = 1 << 62;
const MAX_BOOT_ORDER: usize = 128;

/// A partition the Imager staged, by identity and extent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedIdentity {
    pub partuuid: String,
    pub first_lba: u64,
    pub last_lba: u64,
}

/// An inclusive range of logical blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extent {
    pub first_lba: u64,
    pub last_lba: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub schema: u32,
    pub operation_id: String,
    pub host_os: HostOs,
    pub disk_guid: String,
    pub disk_size_bytes: u64,
    pub staging_esp: StagedIdentity,
    pub staging_root: StagedIdentity,
    pub target_extent: Extent,
    /// `0004`.
    pub owned_boot_entry: String,
    pub original_boot_order: Vec<String>,
    #[serde(deserialize_with = "deserialize_profile")]
    pub profile: serde_json::Map<String, serde_json::Value>,
    /// The install plan this staging was made for, when every choice is made:
    /// the machine installs it with nobody at its screen. Secret-free, like
    /// every plan, so it may travel in the ESP's handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<install_profile_model::plan::Plan>,
}

/// The operating system the Imager ran on, which the person returns to when they cancel the
/// installation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostOs {
    Windows,
    Linux,
}

impl HostOs {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Linux => "Linux",
        }
    }
}

impl StagedIdentity {
    const fn extent(&self) -> Extent {
        Extent {
            first_lba: self.first_lba,
            last_lba: self.last_lba,
        }
    }
}

impl Extent {
    /// How many blocks the range spans.
    pub const fn lbas(self) -> u64 {
        self.last_lba
            .saturating_sub(self.first_lba)
            .saturating_add(1)
    }

    fn valid(self) -> bool {
        self.first_lba > 0 && self.first_lba <= self.last_lba && self.last_lba < MAX_LBA
    }

    const fn overlaps(self, other: Self) -> bool {
        self.first_lba <= other.last_lba && other.first_lba <= self.last_lba
    }

    /// Whether `next` starts on the block after this one ends.
    fn precedes(self, next: Self) -> bool {
        self.last_lba.checked_add(1) == Some(next.first_lba)
    }
}

/// Canonical 8-4-4-4-12 hexadecimal GUID text, in either case.
pub fn is_guid(value: &str) -> bool {
    let groups = value.split('-').collect::<Vec<_>>();
    groups.len() == 5
        && groups
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// An operation, attempt or request id: the canonical lowercase text of a non-nil UUID, as
/// `uuid::Uuid::new_v4().to_string()` writes it.
pub fn is_operation_id(value: &str) -> bool {
    is_guid(value)
        && !value.bytes().any(|byte| byte.is_ascii_uppercase())
        && value.bytes().any(|byte| byte != b'0' && byte != b'-')
}

/// A firmware boot entry id: four uppercase hex digits.
fn is_boot_id(value: &str) -> bool {
    value.len() == 4
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
}

impl Handoff {
    /// The handoff's own consistency: its version, its identities, its sizes and its boot
    /// entry.
    ///
    /// # Errors
    ///
    /// Why the resource is not a handoff this version accepts.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err("unsupported staged installation version".into());
        }
        if !is_guid(&self.operation_id) || !is_guid(&self.disk_guid) {
            return Err("staged installation identity is malformed".into());
        }
        if self.disk_size_bytes == 0 || self.disk_size_bytes >= MAX_LBA {
            return Err("staged installation has no disk size".into());
        }
        for staged in [&self.staging_esp, &self.staging_root] {
            if !is_guid(&staged.partuuid) || !staged.extent().valid() {
                return Err("staged installer partition is malformed".into());
            }
        }
        if !self.target_extent.valid() {
            return Err("staged target space is malformed".into());
        }
        if self
            .staging_esp
            .partuuid
            .eq_ignore_ascii_case(&self.staging_root.partuuid)
        {
            return Err("staged installer partitions share one identity".into());
        }
        let (target, esp, root) = (
            self.target_extent,
            self.staging_esp.extent(),
            self.staging_root.extent(),
        );
        if esp.overlaps(root) || target.overlaps(esp) || target.overlaps(root) {
            return Err("staged target space overlaps the staged installer".into());
        }
        if !target.precedes(esp) || !esp.precedes(root) {
            return Err("staged target space and installer are not contiguous".into());
        }
        if !is_boot_id(&self.owned_boot_entry)
            || self.original_boot_order.len() > MAX_BOOT_ORDER
            || !self.original_boot_order.iter().all(|id| is_boot_id(id))
            || self.original_boot_order.contains(&self.owned_boot_entry)
        {
            return Err("staged firmware state is malformed".into());
        }
        validate_profile(&self.profile)?;
        if let Some(plan) = &self.plan {
            if !plan.complete() {
                return Err(
                    "staged installation's plan still asks the person's devices for a choice"
                        .into(),
                );
            }
            if plan.destination != install_profile_model::plan::SERVER {
                return Err(
                    "only a Tilo Server installs from a staged plan without a person".into(),
                );
            }
        }
        Ok(())
    }

    /// Parse and validate handoff bytes (JSON plus trailing whitespace).
    ///
    /// # Errors
    ///
    /// Why the bytes are too large, malformed or inconsistent.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("staged installation resource is unexpectedly large".into());
        }
        let mut handoff = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| format!("staged installation resource is malformed: {error}"))?;
        handoff.validate()?;
        drop_invalid_hostname(&mut handoff.profile);
        Ok(handoff)
    }

    /// Validated compact JSON padded with spaces to exactly `len` bytes.
    ///
    /// # Errors
    ///
    /// Why the resource is inconsistent, cannot be serialized, or does not fit `len` bytes.
    pub fn to_padded(&self, len: u64) -> Result<Vec<u8>, String> {
        self.validate()?;
        if self
            .profile
            .get("hostname")
            .is_some_and(|value| !value.as_str().is_some_and(valid_hostname))
        {
            return Err("staged installation profile field hostname is malformed".into());
        }
        pad_json(&serde_json::to_vec(self).map_err(|e| e.to_string())?, len)
    }
}

fn deserialize_profile<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<serde_json::Map<String, serde_json::Value>, D::Error> {
    struct ProfileVisitor;
    impl<'de> serde::de::Visitor<'de> for ProfileVisitor {
        type Value = serde_json::Map<String, serde_json::Value>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a secret-free installation profile")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut profile = serde_json::Map::new();
            while let Some(key) = map.next_key::<String>()? {
                if profile.contains_key(&key) {
                    return Err(serde::de::Error::custom("duplicate profile field"));
                }
                let value = map.next_value()?;
                profile.insert(key, value);
            }
            validate_profile(&profile).map_err(serde::de::Error::custom)?;
            Ok(profile)
        }
    }
    deserializer.deserialize_map(ProfileVisitor)
}

/// One RFC-1123 DNS label (1-63 ASCII letters, digits or hyphens, with no
/// leading/trailing hyphen and no surrounding whitespace or control characters).
#[must_use]
pub fn valid_hostname(value: &str) -> bool {
    value.trim() == value && install_profile_model::valid_hostname(value)
}

/// Remove `"hostname"` when it fails the strict DNS-label rule so a read-back
/// handoff or stored staging record loads cleanly and asks for a valid hostname
/// instead of passing an invalid value to any privileged step.
pub fn drop_invalid_hostname(profile: &mut serde_json::Map<String, serde_json::Value>) {
    if profile
        .get("hostname")
        .is_some_and(|value| !value.as_str().is_some_and(valid_hostname))
    {
        profile.remove("hostname");
    }
}

/// Validate the closed, secret-free installer profile shared by both sides.
pub fn validate_profile(
    profile: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    for (key, value) in profile {
        let valid = match key.as_str() {
            "schema" => value.is_i64() || value.is_u64(),
            "hostname" | "username" | "full_name" | "locale" | "keyboard" | "timezone"
            | "filesystem" => value.as_str().is_some_and(|s| s.chars().count() <= 256),
            "purpose" => value
                .as_str()
                .is_some_and(|s| matches!(s, "computer" | "server")),
            "device_label" => value.as_str().is_some_and(|s| {
                s.len() <= 64
                    && (s.is_empty() || !s.trim().is_empty())
                    && !s.chars().any(char::is_control)
            }),
            _ => false,
        };
        if !valid {
            return Err(format!(
                "staged installation profile field {key} is malformed"
            ));
        }
    }
    Ok(())
}

/// `json` followed by ASCII spaces to exactly `len` bytes.
pub fn pad_json(json: &[u8], len: u64) -> Result<Vec<u8>, String> {
    let len = usize::try_from(len).map_err(|e| e.to_string())?;
    if json.len() > len {
        return Err(format!(
            "{} bytes of JSON do not fit the {len}-byte ESP placeholder",
            json.len()
        ));
    }
    let mut out = json.to_vec();
    out.resize(len, b' ');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_are_canonical_lowercase_and_never_nil() {
        assert!(is_operation_id("0f0e0d0c-0b0a-4908-8706-050403020100"));
        assert!(!is_operation_id("0F0E0D0C-0B0A-4908-8706-050403020100"));
        assert!(!is_operation_id("00000000-0000-0000-0000-000000000000"));
        assert!(!is_operation_id("{0f0e0d0c-0b0a-4908-8706-050403020100}"));
        assert!(!is_operation_id("0f0e0d0c0b0a49088706050403020100"));
        assert!(!is_operation_id(""));
    }

    fn handoff() -> Handoff {
        Handoff {
            schema: SCHEMA,
            operation_id: "0f0e0d0c-0b0a-4908-8706-050403020100".into(),
            host_os: HostOs::Linux,
            disk_guid: "11111111-2222-4333-8444-555555555555".into(),
            disk_size_bytes: 1 << 40,
            staging_esp: StagedIdentity {
                partuuid: "aaaaaaaa-0000-4000-8000-000000000001".into(),
                first_lba: 3000,
                last_lba: 3999,
            },
            staging_root: StagedIdentity {
                partuuid: "bbbbbbbb-0000-4000-8000-000000000002".into(),
                first_lba: 4000,
                last_lba: 9999,
            },
            target_extent: Extent {
                first_lba: 2048,
                last_lba: 2999,
            },
            owned_boot_entry: "0104".into(),
            original_boot_order: vec!["0001".into()],
            profile: serde_json::Map::new(),
            plan: None,
        }
    }

    #[test]
    fn profile_metadata_is_closed_bounded_and_duplicate_free() {
        let mut valid = handoff();
        valid.profile = serde_json::from_value(serde_json::json!({
            "schema": 1, "hostname": "desktop", "device_label": "My computer",
            "purpose": "computer"
        }))
        .unwrap();
        let bytes = valid.to_padded(4096).unwrap();
        assert_eq!(Handoff::parse(&bytes).unwrap(), valid);
        for (key, value) in [
            ("password", serde_json::json!("secret")),
            ("purpose", serde_json::json!("other")),
            ("device_label", serde_json::json!(" ")),
            ("device_label", serde_json::json!("é".repeat(33))),
            ("device_label", serde_json::json!("bad\u{0080}")),
            ("hostname", serde_json::json!("a".repeat(257))),
        ] {
            let mut bad = valid.clone();
            bad.profile.insert(key.into(), value);
            assert!(bad.validate().is_err(), "{key}");
            assert!(
                Handoff::parse(&serde_json::to_vec(&bad).unwrap()).is_err(),
                "{key}"
            );
        }
        let json = serde_json::to_string(&valid).unwrap();
        let bad = json.replacen(
            "\"purpose\":\"computer\"",
            "\"purpose\":\"computer\",\"purpose\":\"server\"",
            1,
        );
        assert_ne!(json, bad);
        assert!(Handoff::parse(bad.as_bytes()).is_err());
    }

    fn complete_server_plan() -> install_profile_model::plan::Plan {
        serde_json::from_value(serde_json::json!({
            "schema": 1,
            "plan": "0123456789abcdef0123456789abcdef",
            "revision": 1,
            "destination": "tilo-server",
            "name": "cupboard",
            "network": {"addressing": "dhcp", "addresses": "", "gateway": "", "dns": ""},
            "disk": {"kind": "largest", "identity": ""},
            "unlock": "owner-device",
            "updates": "stable",
            "hosting": [],
            "open": [],
            "token": {"nonce": "00112233445566778899aabbccddeeff",
                      "expires_at_ns": 1_700_000_000_000_000_000_u64}
        }))
        .unwrap()
    }

    #[test]
    fn a_carried_install_plan_is_complete_and_a_server() {
        let mut handoff = handoff();
        handoff.plan = Some(complete_server_plan());
        let bytes = handoff.to_padded(8192).unwrap();
        let parsed = Handoff::parse(&bytes).unwrap();
        assert_eq!(parsed.plan, handoff.plan);

        let mut open = handoff.clone();
        let mut plan = complete_server_plan();
        plan.open = vec!["name".to_owned()];
        plan.name.clear();
        open.plan = Some(plan);
        assert!(
            open.validate().unwrap_err().contains("still asks"),
            "a plan with an open choice is not unattended"
        );
        assert!(Handoff::parse(&serde_json::to_vec(&open).unwrap()).is_err());

        let mut desktop = handoff.clone();
        let mut plan = complete_server_plan();
        plan.destination = "tilo-os".to_owned();
        desktop.plan = Some(plan);
        assert!(desktop.validate().unwrap_err().contains("Tilo Server"));

        let mut unknown = handoff;
        unknown.plan = Some(complete_server_plan());
        let mut value = serde_json::to_value(&unknown).unwrap();
        value["plan"]["purpose"] = "computer".into();
        assert!(Handoff::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn a_written_handoff_reads_back_and_a_gap_or_bad_entry_is_refused_on_both_sides() {
        let written = handoff().to_padded(4096).unwrap();
        assert_eq!(written.len(), 4096);
        assert_eq!(Handoff::parse(&written).unwrap(), handoff());
        assert!(handoff().to_padded(16).is_err());

        let mut gap = handoff();
        gap.target_extent.last_lba -= 1;
        assert!(gap.validate().unwrap_err().contains("contiguous"));
        assert!(gap.to_padded(4096).is_err(), "the writer refuses it too");

        let mut lowercase = handoff();
        lowercase.owned_boot_entry = "00ab".into();
        assert!(lowercase.validate().unwrap_err().contains("firmware"));
    }

    #[test]
    fn hostnames_accept_valid_dns_labels_up_to_63_chars() {
        for good in ["a", "desktop", "My-Host-01", &"a".repeat(63)] {
            let mut h = handoff();
            h.profile.insert("hostname".into(), good.into());
            let bytes = h.to_padded(4096).unwrap();
            assert_eq!(Handoff::parse(&bytes).unwrap(), h, "{good}");
        }

        // Building a new handoff refuses invalid hostnames, while reading a staged handoff
        // accepts bounded text (<= 256 chars) and drops the invalid hostname for repair.
        for bad in [
            "",
            "bad host",
            "-desktop",
            "desktop-",
            " desktop",
            "desktop ",
            "desktop\n",
            "desktop\r",
            "desktop\t",
            "desktop\u{0000}",
            "desktop.local",
            &"h".repeat(64),
            &"h".repeat(256),
        ] {
            let mut h = handoff();
            h.profile.insert("hostname".into(), bad.into());
            assert!(h.to_padded(4096).is_err(), "write refuses {bad:?}");
            let encoded = serde_json::to_vec(&h).unwrap();
            let parsed = Handoff::parse(&encoded).unwrap();
            assert!(
                !parsed.profile.contains_key("hostname"),
                "read drops {bad:?}"
            );
        }

        // The 256-character bound and string type remain enforced on read.
        let mut oversized = handoff();
        oversized
            .profile
            .insert("hostname".into(), "h".repeat(257).into());
        assert!(Handoff::parse(&serde_json::to_vec(&oversized).unwrap()).is_err());
        let mut non_string = handoff();
        non_string.profile.insert("hostname".into(), 42.into());
        assert!(Handoff::parse(&serde_json::to_vec(&non_string).unwrap()).is_err());
    }
}
