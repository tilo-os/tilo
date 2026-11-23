//! Pure validation of public transport metadata; no metadata grants source access.

use prepared_setup_model::{
    ApplicationsExpectation, BrowserExpectation, HomeExpectation, Limits,
    stored::{StoredApplicationsExpectation, StoredBrowserExpectation, StoredHomeExpectation},
};
use serde_json::Value;

use super::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayloadExpectation {
    Home(HomeExpectation),
    Applications(ApplicationsExpectation),
    Browser(BrowserExpectation),
}

impl PayloadExpectation {
    pub fn operation(&self) -> &str {
        match self {
            Self::Home(e) => &e.operation,
            Self::Applications(e) => &e.operation,
            Self::Browser(e) => &e.operation,
        }
    }

    pub fn filename(&self) -> &str {
        match self {
            Self::Home(e) => &e.filename,
            Self::Applications(e) => &e.filename,
            Self::Browser(e) => &e.filename,
        }
    }

    pub fn setup_binding(&self) -> &str {
        match self {
            Self::Home(e) => &e.setup_binding,
            Self::Applications(e) => &e.setup_binding,
            Self::Browser(e) => &e.setup_binding,
        }
    }

    pub fn sealed_bytes(&self) -> u64 {
        match self {
            Self::Home(e) => e.sealed_bytes,
            Self::Applications(e) => e.sealed_bytes,
            Self::Browser(e) => e.sealed_bytes,
        }
    }

    pub fn plaintext_bytes(&self) -> u64 {
        match self {
            Self::Home(e) => e.plaintext_bytes,
            Self::Applications(e) => e.plaintext_bytes,
            Self::Browser(e) => e.plaintext_bytes,
        }
    }

    pub fn entries(&self) -> usize {
        match self {
            Self::Home(e) => e.entries,
            Self::Applications(e) => e.entries,
            Self::Browser(e) => e.entries,
        }
    }

    pub fn metadata_bytes(&self) -> usize {
        match self {
            Self::Home(e) => e.limits.metadata_bytes,
            Self::Applications(e) => e.limits.metadata_bytes,
            Self::Browser(e) => e.limits.metadata_bytes,
        }
    }

    pub fn ciphertext_sha256(&self) -> &[u8; 32] {
        match self {
            Self::Home(e) => &e.ciphertext_sha256,
            Self::Applications(e) => &e.ciphertext_sha256,
            Self::Browser(e) => &e.ciphertext_sha256,
        }
    }

    pub fn is_browser(&self) -> bool {
        matches!(self, Self::Browser(_))
    }

    pub fn is_applications(&self) -> bool {
        matches!(self, Self::Applications(_))
    }

    pub fn to_value(&self) -> Result<Value> {
        match self {
            Self::Home(e) => serde_json::to_value(e),
            Self::Applications(e) => serde_json::to_value(e),
            Self::Browser(e) => serde_json::to_value(e),
        }
        .map_err(|_| invalid("metadata JSON is invalid"))
    }
}

fn invalid(_message: &str) -> std::io::Error {
    super::invalid()
}

const MAX_BYTES: u64 = 32 * 1024 * 1024 + 16384;
const SIGNED_MAX: u64 = i64::MAX as u64;

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value
        .get(key)
        .ok_or_else(|| invalid("required metadata field is missing"))
}

fn number(value: &Value, key: &str) -> Result<u64> {
    field(value, key)?
        .as_u64()
        .ok_or_else(|| invalid("metadata number is invalid"))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    field(value, key)?
        .as_str()
        .ok_or_else(|| invalid("metadata string is invalid"))
}

fn keys(value: &Value, expected: &[&str]) -> Result<()> {
    require(
        value.as_object().is_some_and(|object| {
            object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
        }),
        "metadata fields differ",
    )
}

fn absent(value: &Value, key: &str) -> bool {
    value.get(key).is_none_or(Value::is_null)
}

fn bytes(value: &Value, nonzero: bool) -> bool {
    value.as_array().is_some_and(|array| {
        array.len() == 32
            && array.iter().all(|n| n.as_u64().is_some_and(|n| n <= 255))
            && (!nonzero || array.iter().any(|n| n.as_u64().is_some_and(|n| n != 0)))
    })
}

fn hex_digest(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

fn canonical_identity(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .is_ok_and(|id| !id.is_nil() && id.hyphenated().to_string() == value)
}

fn check_signed_and_sealed(
    sealed_bytes: u64,
    plaintext_bytes: u64,
    entries: usize,
    limits: Limits,
) -> Result<()> {
    require(
        (48..=SIGNED_MAX).contains(&sealed_bytes)
            && plaintext_bytes <= SIGNED_MAX
            && u64::try_from(entries).is_ok_and(|n| n <= SIGNED_MAX)
            && limits.plaintext_bytes <= SIGNED_MAX
            && u64::try_from(limits.entries).is_ok_and(|n| n <= SIGNED_MAX)
            && u64::try_from(limits.metadata_bytes).is_ok_and(|n| n <= SIGNED_MAX),
        "portable Home receipt length is invalid",
    )
}

pub fn expectation(value: &Value, kind: &str) -> Result<PayloadExpectation> {
    match kind {
        "home" => {
            let parsed = serde_json::from_value::<StoredHomeExpectation>(value.clone())
                .map_err(|_| invalid("portable Home receipt differs"))?
                .0;
            parsed
                .validate_installer()
                .map_err(|_| invalid("portable Home receipt is invalid"))?;
            check_signed_and_sealed(
                parsed.sealed_bytes,
                parsed.plaintext_bytes,
                parsed.entries,
                parsed.limits,
            )?;
            require(
                parsed.kind.runtime_sockets <= SIGNED_MAX,
                "portable Home receipt length is invalid",
            )?;
            Ok(PayloadExpectation::Home(parsed))
        }
        "applications" => {
            let parsed = serde_json::from_value::<StoredApplicationsExpectation>(value.clone())
                .map_err(|_| invalid("portable applications receipt differs"))?
                .0;
            parsed
                .validate_installer()
                .map_err(|_| invalid("portable applications receipt is invalid"))?;
            check_signed_and_sealed(
                parsed.sealed_bytes,
                parsed.plaintext_bytes,
                parsed.entries,
                parsed.limits,
            )?;
            Ok(PayloadExpectation::Applications(parsed))
        }
        "browser" => {
            let parsed = serde_json::from_value::<StoredBrowserExpectation>(value.clone())
                .map_err(|_| invalid("portable browser receipt differs"))?
                .0;
            parsed
                .validate_installer()
                .map_err(|_| invalid("portable browser receipt is invalid"))?;
            check_signed_and_sealed(
                parsed.sealed_bytes,
                parsed.plaintext_bytes,
                parsed.entries,
                parsed.limits,
            )?;
            Ok(PayloadExpectation::Browser(parsed))
        }
        _ => Err(invalid("unsupported portable payload kind")),
    }
}

pub fn receipt(value: &Value, size: u64, digest: &str) -> Result<()> {
    keys(value, &["schema", "sealed_bytes", "ciphertext_sha256"])?;
    require(
        number(value, "schema")? == 1
            && (48..=MAX_BYTES).contains(&number(value, "sealed_bytes")?)
            && hex_digest(field(value, "ciphertext_sha256")?)
            && number(value, "sealed_bytes")? == size
            && text(value, "ciphertext_sha256")? == digest,
        "expected prepared setup ciphertext is missing, changed or malformed",
    )
}

pub fn origin(value: &Value) -> Result<()> {
    keys(
        value,
        &[
            "schema",
            "identity_public_key",
            "setup_binding",
            "sealed_bytes",
            "ciphertext_sha256",
        ],
    )?;
    let binding = text(value, "setup_binding")?;
    require(
        number(value, "schema")? == 1
            && bytes(field(value, "identity_public_key")?, true)
            && !binding.is_empty()
            && binding.len() <= 256
            && !binding.chars().any(|c| c < ' ' || c == '\u{7f}')
            && (48..=MAX_BYTES).contains(&number(value, "sealed_bytes")?)
            && hex_digest(field(value, "ciphertext_sha256")?),
        "prepared first-use origin identity or ciphertext differs",
    )
}

fn scope_fields<'a>(authenticated: &Value, scope: &'a Value) -> Result<&'a str> {
    keys(
        scope,
        &[
            "schema",
            "source_operation",
            "applications_operation",
            "staging_operation",
            "staging_plan_sha256",
            "setup_binding",
            "identity_public_key",
        ],
    )?;
    require(
        number(scope, "schema")? == 1,
        "Computer applications scope schema differs",
    )?;
    for key in [
        "source_operation",
        "applications_operation",
        "staging_operation",
    ] {
        require(
            staging_handoff_model::is_operation_id(text(scope, key)?),
            "Computer applications operation is not canonical",
        )?;
    }
    let binding = text(scope, "setup_binding")?;
    require(
        hex_digest(field(scope, "staging_plan_sha256")?)
            && bytes(field(scope, "identity_public_key")?, false)
            && scope.get("identity_public_key") == authenticated.get("identity_public_key")
            && (1..=256).contains(&binding.len())
            && !binding.chars().any(char::is_control)
            && scope.get("setup_binding") == authenticated.get("setup_binding"),
        "Computer applications person or setup differs",
    )?;
    Ok(binding)
}

fn scope_plan(authenticated: &Value, scope: &Value) -> Result<()> {
    let plan = field(authenticated, "retained_home")?;
    require(
        plan.is_object()
            && plan.get("home_policy").and_then(Value::as_str) == Some("access-read-write")
            && plan.get("categories").is_some_and(Value::is_object)
            && plan
                .get("categories")
                .and_then(|v| v.get("home"))
                .and_then(Value::as_bool)
                == Some(true)
            && plan.get("operation_id") == scope.get("source_operation"),
        "Computer applications source differs",
    )
}

pub fn shared_apps(value: &Value) -> Result<bool> {
    let required = !absent(value, "retained_home")
        && value
            .get("completed_applications_only")
            .and_then(Value::as_bool)
            == Some(true);
    require(
        absent(value, "computer_apps_scope") != required,
        "Computer applications scope is missing or unexpected",
    )?;
    if !required {
        return Ok(false);
    }
    let scope = field(value, "computer_apps_scope")?;
    scope_fields(value, scope)?;
    scope_plan(value, scope)?;
    let applications = field(value, "completed_applications")?;
    require(
        applications.is_object()
            && applications.get("operation") == scope.get("applications_operation")
            && absent(value, "completed_home"),
        "Computer applications source or completed output differs",
    )?;
    Ok(true)
}

fn pending_scope(value: &Value) -> Result<()> {
    if absent(value, "pending_computer_applications_scope") {
        return Ok(());
    }
    let scope = field(value, "pending_computer_applications_scope")?;
    scope_fields(value, scope)?;
    let source = text(scope, "source_operation")?;
    let apps = text(scope, "applications_operation")?;
    let staging = text(scope, "staging_operation")?;
    require(
        source != apps && source != staging && apps != staging,
        "pending Computer operations overlap",
    )?;
    scope_plan(value, scope)?;
    let plan = field(value, "retained_home")?;
    require(
        plan.get("categories")
            .and_then(|v| v.get("flatpak"))
            .and_then(Value::as_bool)
            == Some(false)
            && value
                .get("completed_applications_only")
                .is_none_or(|v| v.as_bool() == Some(false))
            && [
                "completed_home",
                "completed_applications",
                "completed_setup",
                "computer_apps_scope",
            ]
            .iter()
            .all(|key| absent(value, key)),
        "pending Computer source or purpose differs",
    )
}

fn reported_resize(value: &Value) -> Result<()> {
    if absent(value, "planned_btrfs_resize") {
        return Ok(());
    }
    let resize = field(value, "planned_btrfs_resize")?;
    keys(
        resize,
        &[
            "schema",
            "original_plan_sha256",
            "reviewed_plan_sha256",
            "reviewed_plan",
            "filesystem_uuid",
            "partition_guid",
            "device_id",
            "from_bytes",
            "to_bytes",
            "new_last_lba",
        ],
    )?;
    require(
        number(resize, "schema")? == 1 && number(resize, "device_id")? == 1,
        "reported resize schema differs",
    )?;
    for key in ["original_plan_sha256", "reviewed_plan_sha256"] {
        require(
            bytes(field(resize, key)?, true),
            "reported resize digest is invalid",
        )?;
    }
    for key in ["filesystem_uuid", "partition_guid"] {
        require(
            canonical_identity(text(resize, key)?),
            "reported resize identity is not canonical",
        )?;
    }
    let review = field(resize, "reviewed_plan")?;
    require(
        review.is_object() && json_size(review)? <= 16 * 1024,
        "reported resize review exceeds bounds",
    )?;
    let from = number(resize, "from_bytes")?;
    let to = number(resize, "to_bytes")?;
    let new_last = number(resize, "new_last_lba")?;
    let volume = value
        .get("retained_home")
        .and_then(|v| v.get("home"))
        .and_then(|v| v.get("volume"))
        .filter(|v| v.is_object())
        .ok_or_else(|| invalid("reported resize lacks effective retained source"))?;
    let first = number(volume, "first_lba")?;
    let last = number(volume, "last_lba")?;
    let sector = number(volume, "sector_bytes")?;
    let extent_bytes = u128::from(last)
        .checked_sub(u128::from(first))
        .and_then(|n| n.checked_add(1))
        .and_then(|n| n.checked_mul(u128::from(sector)));
    require(
        (512..=65536).contains(&sector),
        "reported resize sector size is out of bounds",
    )?;
    require(
        sector.is_power_of_two()
            && last >= first
            && last == new_last
            && to > 0
            && to < from
            && to % (1 << 20) == 0
            && extent_bytes == Some(u128::from(to))
            && volume.get("filesystem").and_then(Value::as_str) == Some("btrfs")
            && text(volume, "filesystem_uuid")?.to_lowercase() == text(resize, "filesystem_uuid")?
            && text(volume, "partition_guid")?.to_lowercase() == text(resize, "partition_guid")?,
        "reported resize differs from effective retained source",
    )
}

fn json_size(value: &Value) -> Result<usize> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| invalid("metadata JSON is invalid"))
}

pub fn authenticated_metadata(value: &Value) -> Result<()> {
    require(
        value.is_object() && number(value, "schema")? == 1,
        "authenticated setup receipt differs",
    )?;
    require(
        json_size(value)? <= 80 * 1024,
        "authenticated setup receipt exceeds bounds",
    )?;
    require(
        value
            .get("completed_applications_only")
            .is_none_or(Value::is_boolean),
        "completed applications purpose is malformed",
    )?;
    shared_apps(value)?;
    pending_scope(value)?;
    reported_resize(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prepared_setup_model::{Applications, Browser, Home};
    use serde_json::json;

    fn home() -> Value {
        let operation = "portable-home".to_owned();
        serde_json::to_value(HomeExpectation {
            filename: HomeExpectation::filename(&operation).unwrap(),
            operation,
            setup_binding: "é".repeat(256),
            sealed_bytes: 48,
            plaintext_bytes: 1,
            entries: 1,
            ciphertext_sha256: [0; 32],
            limits: Limits {
                plaintext_bytes: 1,
                entries: 1,
                metadata_bytes: 1,
            },
            kind: Home { runtime_sockets: 0 },
        })
        .unwrap()
    }

    tilo_test_env::scenario! {
        fn receipts_and_envelopes_are_closed_and_bounded;

        phase portable_receipt_preserves_types_closed_fields_and_character_bound;
        {
            let value = home();
            let parsed_home = expectation(&value, "home").unwrap();
            for (key, replacement) in [
                ("sealed_bytes", json!(47)),
                ("sealed_bytes", json!(48.0)),
                ("entries", json!(-1)),
                ("ciphertext_sha256", json!(vec![256; 32])),
                ("setup_binding", json!("é".repeat(257))),
                ("kind", json!({"runtime_sockets": 2})),
                ("kind", json!({"runtime_sockets": 0, "extra": 1})),
            ] {
                let mut bad = value.clone();
                bad[key] = replacement;
                assert!(expectation(&bad, "home").is_err(), "{key}");
            }
            let mut next_line = value.clone();
            next_line["setup_binding"] = json!("before\u{85}after");
            assert!(expectation(&next_line, "home").is_ok());

            let mut extra = value.clone();
            extra["extra"] = Value::Null;
            assert!(expectation(&extra, "home").is_err());
            assert!(expectation(&value, "unsupported").is_err());

            // Earlier flat shape is accepted and decodes to the same expectation as kind.
            let mut flat_home = json!({
                "schema": 1, "operation": "portable-home", "purpose": "home",
                "filename": "home-portable-home.sealed", "setup_binding": "é".repeat(256),
                "sealed_bytes": 48, "plaintext_bytes": 1, "entries": 1,
                "ciphertext_sha256": vec![0; 32], "runtime_sockets": 0,
                "limits": {"plaintext_bytes": 1, "entries": 1, "metadata_bytes": 1}
            });
            assert_eq!(expectation(&flat_home, "home").unwrap(), parsed_home);
            for (key, replacement) in [
                ("schema", json!(true)),
                ("sealed_bytes", json!(48.0)),
                ("entries", json!(-1)),
                ("runtime_sockets", json!(2)),
                ("ciphertext_sha256", json!(vec![256; 32])),
                ("setup_binding", json!("é".repeat(257))),
            ] {
                let mut bad = flat_home.clone();
                bad[key] = replacement;
                assert!(expectation(&bad, "home").is_err(), "flat {key}");
            }
            flat_home["extra"] = Value::Null;
            assert!(expectation(&flat_home, "home").is_err());

            let app_op = "11111111-1111-4111-8111-111111111111".to_owned();
            let mut apps = ApplicationsExpectation {
                filename: ApplicationsExpectation::filename(&app_op).unwrap(),
                operation: app_op.clone(),
                setup_binding: "binding".into(),
                sealed_bytes: 48,
                plaintext_bytes: 1,
                entries: 1,
                ciphertext_sha256: [0; 32],
                limits: Limits {
                    plaintext_bytes: 1,
                    entries: 1,
                    metadata_bytes: 1,
                },
                kind: Applications {
                    consumer_protocol: 1,
                    manifest_sha256: [1; 32],
                },
            };
            let parsed_apps_v1 =
                expectation(&serde_json::to_value(&apps).unwrap(), "applications").unwrap();
            let mut flat_apps = json!({
                "schema": 1, "operation": app_op, "purpose": "flatpak_applications",
                "filename": apps.filename, "setup_binding": "binding",
                "sealed_bytes": 48, "plaintext_bytes": 1, "entries": 1,
                "ciphertext_sha256": vec![0; 32], "manifest_sha256": vec![1; 32],
                "limits": {"plaintext_bytes": 1, "entries": 1, "metadata_bytes": 1}
            });
            assert_eq!(
                expectation(&flat_apps, "applications").unwrap(),
                parsed_apps_v1
            );
            flat_apps["consumer_protocol"] = json!(2);
            assert!(expectation(&flat_apps, "applications").is_err());
            apps.kind.consumer_protocol = 2;
            assert!(expectation(&serde_json::to_value(&apps).unwrap(), "applications").is_ok());
            apps.kind.consumer_protocol = 3;
            assert!(expectation(&serde_json::to_value(&apps).unwrap(), "applications").is_err());

            let browser_op = "22222222-2222-4222-8222-222222222222".to_owned();
            let mut browser = BrowserExpectation {
                filename: BrowserExpectation::filename(&browser_op).unwrap(),
                operation: browser_op.clone(),
                setup_binding: "binding".into(),
                sealed_bytes: 48,
                plaintext_bytes: 1,
                entries: 1,
                ciphertext_sha256: [0; 32],
                limits: Limits {
                    plaintext_bytes: 1,
                    entries: 1,
                    metadata_bytes: 1,
                },
                kind: Browser {
                    plan_sha256: [2; 32],
                },
            };
            let parsed_browser =
                expectation(&serde_json::to_value(&browser).unwrap(), "browser").unwrap();
            let flat_browser = json!({
                "schema": 1, "operation": browser_op, "purpose": "browser_profiles",
                "filename": browser.filename, "setup_binding": "binding",
                "sealed_bytes": 48, "plaintext_bytes": 1, "entries": 1,
                "ciphertext_sha256": vec![0; 32], "plan_sha256": vec![2; 32],
                "limits": {"plaintext_bytes": 1, "entries": 1, "metadata_bytes": 1}
            });
            assert_eq!(expectation(&flat_browser, "browser").unwrap(), parsed_browser);
            browser.limits.entries = 129;
            assert!(expectation(&serde_json::to_value(&browser).unwrap(), "browser").is_err());
        }

        phase origin_and_receipt_have_distinct_identity_and_length_rules;
        {
            let digest = "a".repeat(64);
            let sealed = json!({"schema": 1, "sealed_bytes": 48, "ciphertext_sha256": digest});
            assert!(receipt(&sealed, 48, &digest).is_ok());
            assert!(receipt(&sealed, 49, &digest).is_err());
            assert!(receipt(&sealed, 48, &"A".repeat(64)).is_err());
            let mut record = sealed;
            record["identity_public_key"] = json!(vec![1; 32]);
            record["setup_binding"] = json!("é".repeat(128));
            assert!(origin(&record).is_ok());
            record["setup_binding"] = json!("é".repeat(129));
            assert!(origin(&record).is_err());
            record["setup_binding"] = json!("binding");
            record["identity_public_key"] = json!(vec![0; 32]);
            assert!(origin(&record).is_err());
        }

        phase envelope_checks_only_metadata_but_rejects_wrong_types_and_missing_scope;
        {
            assert!(
                authenticated_metadata(&json!({"schema": 1, "completed_home": "not checked here"}))
                    .is_ok()
            );
            assert!(
                authenticated_metadata(&json!({"schema": 1, "completed_applications_only": null}))
                    .is_err()
            );
            assert!(authenticated_metadata(&json!({"schema": true})).is_err());
            assert!(
                authenticated_metadata(&json!({"schema": 1, "extra": "x".repeat(80 * 1024)})).is_err()
            );
            assert!(
                shared_apps(&json!({"retained_home": {}, "completed_applications_only": true}))
                    .is_err()
            );
            assert!(authenticated_metadata(&json!({"schema": 1, "planned_btrfs_resize": {}})).is_err());
            assert!(
                authenticated_metadata(
                    &json!({"schema": 1, "pending_computer_applications_scope": {}})
                )
                .is_err()
            );
        }
    }
}
