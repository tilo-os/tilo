//! Bind optional service-session admission to actual signed receiving artifacts.
//!
//! Candidate receiver qualification binds the native CEF qualifier to signed
//! artifacts. Admission remains unavailable until the actual sandbox credential
//! receiving route is qualified; codec/native-engine probes alone are
//! insufficient.
use super::{
    account::{self, Runner, digest, hex_digest, interpreter, lexists, load_toml, read_trimmed},
    field, insert, invalid,
    invoke::{self, ENV},
    json, require, under,
};
use crate::account_admission::RECEIVING_DEPENDENCIES;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

const MIB: u64 = 1024 * 1024;
const RECEIVER_BOUND: u64 = 256 * MIB;
const PROBE_BOUND: u64 = 64 * MIB;
const LIBRARY_BOUND: u64 = 2048 * MIB;
pub const RECEIVING_ENTRY: &str = "/usr/libexec/tilo-installer-receive-fixture/receive.py";
pub const QUALIFICATION_SCRIPT: &str = "/usr/libexec/tilo-service-session-qualification.py";
const BRIDGE: &str = "/usr/libexec/tilo-installer-receive-fixture/service_contract.py";
const BRIDGE_OUTPUT_BOUND: usize = 16 * 1024;
const SHELLS: [&str; 2] = ["/usr/bin/shell", "/usr/bin/tilo-shell"];
/// Schema 2 is the complete measured Python receiving closure. Schema 1
/// candidate Browser/CEF proofs are never sufficient for service admission.
pub const SERVICE_SCHEMA: i64 = 2;
pub const ROUTE_KEYS: [&str; 7] = [
    "receiving_shell_path",
    "receiving_shell_sha256",
    "receiving_entry_sha256",
    "receiving_dependencies",
    "receiving_python_path",
    "receiving_python_sha256",
    "receiving_route_qualification_sha256",
];

/// Native CEF qualification executor: command and environment.
pub type Qualifier<'a> =
    dyn FnMut(&[OsString], &[(&str, OsString)]) -> std::io::Result<Vec<u8>> + 'a;
/// Owned receiving-route executor: command, environment and request.
pub type RouteQualifier<'a> =
    dyn FnMut(&[OsString], &[(&str, OsString)], &[u8]) -> std::io::Result<Vec<u8>> + 'a;

/// The account receipt fields the service proof binds to, named as there.
#[allow(
    clippy::struct_field_names,
    reason = "the fields keep the account receipt's names"
)]
pub struct Sdk {
    pub runtime_version: String,
    pub runtime_root_hash: String,
    pub runtime_probe_sha256: String,
    pub runtime_library_sha256: String,
    pub runtime_library: String,
}
impl Sdk {
    pub fn from_receipt(receipt: &Value) -> std::io::Result<Self> {
        let field = |name: &str| {
            receipt[name]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("account receipt lacks SDK identity"))
        };
        Ok(Self {
            runtime_version: field("runtime_version")?,
            runtime_root_hash: field("runtime_root_hash")?,
            runtime_probe_sha256: field("runtime_probe_sha256")?,
            runtime_library_sha256: field("runtime_library_sha256")?,
            runtime_library: field("runtime_library")?,
        })
    }
}

fn response(output: &[u8], browser: bool) -> std::io::Result<i64> {
    let value = json::strict(output)?;
    let protocol = value
        .get("service_session_protocol")
        .and_then(Value::as_i64)
        .filter(|protocol| matches!(protocol, 1 | 2))
        .ok_or_else(|| invalid("unsupported service protocol"))?;
    let mut expected =
        json!({"schema": 1, "service_session_protocol": protocol, "codec_verified": true});
    if browser {
        insert(&mut expected, "custody_binding_verified", true);
        insert(&mut expected, "resource_action_verified", true);
    }
    require(
        value == expected,
        "receiving service capability probe failed",
    )?;
    Ok(protocol)
}

fn native_origin_completion(output: &[u8]) -> std::io::Result<()> {
    require(
        json::strict(output)?
            == json!({"schema": 1, "service_session_protocol": 2,
                      "cookie_persistence_verified": true,
                      "origin_storage_persistence_verified": true}),
        "native CEF origin persistence qualification did not complete",
    )
}

/// The image producer is root, not the Browser user. Exercise CEF as an
/// unprivileged synthetic actor so its native sandbox remains enabled; neither
/// a `--no-sandbox` flag nor root credentials belong in this probe.
pub fn native_qualify(command: &[OsString], env: &[(&str, OsString)]) -> std::io::Result<Vec<u8>> {
    let command = privilege_dropped(command, rustix::process::geteuid().is_root());
    invoke::run(&invoke::Request {
        command: &command,
        env,
        input: None,
        limit: 4096,
        timeout: Duration::from_secs(300),
    })
}

fn privilege_dropped(command: &[OsString], root: bool) -> Vec<OsString> {
    if !root {
        return command.to_vec();
    }
    let mut dropped: Vec<OsString> = [
        "/usr/bin/setpriv",
        "--reuid=65534",
        "--regid=65534",
        "--clear-groups",
        "--",
    ]
    .map(OsString::from)
    .to_vec();
    dropped.extend_from_slice(command);
    dropped
}

/// The selected installed receiving Browser and its exact CEF runtime.
pub struct Selected {
    pub package_id_hex: String,
    pub publisher_key_hex: String,
    runtime: Option<String>,
    pub browser: PathBuf,
    pub version: String,
    pub bundle: PathBuf,
}

/// Canonical installed-store layout, never a caller-supplied executable path.
pub fn selected(root: &Path) -> std::io::Result<Option<Selected>> {
    let registry = root.join("usr/share/tilo-apps/registry.toml");
    if !registry.exists() {
        return Ok(None);
    }
    let registry = load_toml(&registry)?;
    let entries = match registry.get("entry") {
        None => &Vec::new(),
        Some(entries) => entries
            .as_array()
            .ok_or_else(|| invalid("invalid app registry"))?,
    };
    let browser = entries
        .iter()
        .filter(|entry| entry.get("id").and_then(toml::Value::as_str) == Some("so.tilo.browser"))
        .collect::<Vec<_>>();
    require(browser.len() <= 1, "ambiguous receiving Browser package")?;
    let Some(row) = browser.first() else {
        return Ok(None);
    };
    let text = |name: &str| row.get(name).and_then(toml::Value::as_str);
    let (Some(package_id), Some(publisher)) = (text("package_id_hex"), text("publisher_key_hex"))
    else {
        return Err(invalid("Browser package has no signed identity"));
    };
    require(
        hex_digest(package_id) && hex_digest(publisher),
        "Browser package has no signed identity",
    )?;
    let payload = format!("/usr/share/tilo-apps/pkgs/{package_id}");
    let metadata = format!("{payload}.pkg/manifest.toml");
    require(
        text("payload_root") == Some(payload.as_str())
            && text("manifest_path") == Some(metadata.as_str()),
        "Browser package does not use the canonical image store",
    )?;
    let manifest = load_toml(&under(root, &metadata))?;
    let empty = toml::Table::new();
    let package = match manifest.get("package") {
        None => &empty,
        Some(package) => package
            .as_table()
            .ok_or_else(|| invalid("receiving manifest is not Browser"))?,
    };
    require(
        package.get("id").and_then(toml::Value::as_str) == Some("so.tilo.browser"),
        "receiving manifest is not Browser",
    )?;
    let engines = package
        .get("optional-runtimes")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .filter_map(|value| value.strip_prefix("tilo-runtime/"))
        .filter(|value| value.ends_with("-chromium"))
        .collect::<Vec<_>>();
    let [version] = engines.as_slice() else {
        return Err(invalid("Browser must name one exact receiving CEF runtime"));
    };
    let version = (*version).to_owned();
    require(
        account::version_ok(&version),
        "unsafe receiving CEF version",
    )?;
    let bundle = root.join("usr/lib/tilo-runtime").join(&version);
    if !bundle.exists() {
        return Ok(None);
    }
    require(
        manifest
            .get("entrypoint")
            .and_then(|entry| entry.get("bin"))
            .and_then(toml::Value::as_str)
            == Some("bin/browser"),
        "receiving Browser executable differs from canonical entrypoint",
    )?;
    Ok(Some(Selected {
        package_id_hex: package_id.to_owned(),
        publisher_key_hex: publisher.to_owned(),
        runtime: package
            .get("runtime")
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
        browser: under(root, &payload).join("bin/browser"),
        version,
        bundle,
    }))
}

fn executable(path: &Path, limit: u64, message: &str) -> std::io::Result<()> {
    require(account::regular(path, limit)?.mode() & 0o111 != 0, message)
}

/// Measure the fixed receiving Shell and Python closure.
pub fn route_artifacts(root: &Path, shell_path: &str) -> std::io::Result<Map<String, Value>> {
    require(
        SHELLS.contains(&shell_path),
        "receiving Shell is not a canonical packaged receiver",
    )?;
    let shell = under(root, shell_path);
    executable(&shell, RECEIVER_BOUND, "receiving Shell is not executable")?;
    let mut dependencies = Map::new();
    for relative in RECEIVING_DEPENDENCIES {
        let path = under(root, relative);
        account::regular(&path, MIB)?;
        dependencies.insert((*relative).into(), digest(&path, MIB)?.into());
    }
    // Python's fixed installed alias may target its versioned ELF, but may not
    // redirect outside the admitted image. The target is measured independently.
    let python = std::fs::canonicalize(root.join("usr/bin/python3"))?;
    let name = python
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    require(
        python.parent() == Some(root.join("usr/bin").as_path())
            && name.strip_prefix("python3").is_some_and(|suffix| {
                suffix.is_empty()
                    || suffix.strip_prefix('.').is_some_and(|minor| {
                        !minor.is_empty() && minor.bytes().all(|b| b.is_ascii_digit())
                    })
            }),
        "receiving Python escapes the canonical image interpreter identity",
    )?;
    executable(
        &python,
        RECEIVER_BOUND,
        "receiving Python is not executable",
    )?;
    let mut facts = Map::new();
    facts.insert(
        "receiving_shell_sha256".into(),
        digest(&shell, RECEIVER_BOUND)?.into(),
    );
    facts.insert(
        "receiving_entry_sha256".into(),
        dependencies
            .get(RECEIVING_ENTRY)
            .cloned()
            .unwrap_or_default(),
    );
    facts.insert("receiving_dependencies".into(), Value::Object(dependencies));
    facts.insert(
        "receiving_python_path".into(),
        format!("/usr/bin/{name}").into(),
    );
    facts.insert(
        "receiving_python_sha256".into(),
        digest(&python, RECEIVER_BOUND)?.into(),
    );
    let mut hash = Sha256::new();
    hash.update(b"tilo-installed-service-receiver-python-v1\0");
    hash.update(json::canonical_ascii(&Value::Object(facts.clone())));
    facts.insert(
        "receiving_route_qualification_sha256".into(),
        hex::encode(hash.finalize()).into(),
    );
    Ok(facts)
}

/// Inputs of one optional service proof run.
pub struct Produce<'a, 'r> {
    pub root: &'a Path,
    pub runner: &'a mut Runner<'r>,
    pub sdk: &'a Sdk,
    pub qualify: bool,
    pub evidence: Option<&'a Value>,
    pub qualifier: &'a mut Qualifier<'r>,
    pub route_qualifier: &'a mut RouteQualifier<'r>,
}

/// A saved receipt attests prior native qualification: a read-only check
/// revalidates its artifacts without repeating TLS/enrollment or rewriting.
pub fn produce(run: Produce<'_, '_>) -> std::io::Result<Option<Value>> {
    let Produce {
        root,
        runner,
        sdk,
        qualify,
        evidence,
        qualifier,
        route_qualifier,
    } = run;
    if let Some(evidence) = evidence {
        let fields = evidence
            .as_object()
            .filter(|fields| ROUTE_KEYS.iter().all(|key| fields.contains_key(*key)))
            .ok_or_else(|| {
                invalid("saved service proof lacks qualified sandbox credential receiving route")
            })?;
        require(
            fields.get("schema").and_then(Value::as_i64) == Some(SERVICE_SCHEMA),
            "unsupported measured receiving closure schema",
        )?;
        let mut candidate_evidence = fields.clone();
        candidate_evidence.retain(|key, _| !ROUTE_KEYS.contains(&key.as_str()));
        candidate_evidence.insert("schema".into(), 1.into());
        let candidate = qualify_receivers(
            root,
            runner,
            sdk,
            false,
            Some(&Value::Object(candidate_evidence)),
            qualifier,
        )?
        .ok_or_else(|| invalid("saved service consumer is no longer installed"))?;
        let shell_path = field(evidence, "receiving_shell_path")
            .as_str()
            .unwrap_or_default();
        let facts = route_artifacts(root, shell_path)?;
        let proof = route_proof(candidate, shell_path, facts);
        require(
            &proof == evidence,
            "installed receiving route differs from qualified consumers",
        )?;
        return Ok(Some(proof));
    }
    if qualify {
        // Cloud grants do not imply an installed, qualified service-session
        // receiver. Only explicitly provisioned qualification tools enable
        // this optional proof; a saved proof above may never disappear.
        let present =
            [RECEIVING_ENTRY, QUALIFICATION_SCRIPT].map(|path| lexists(&under(root, path)));
        require(
            present[0] == present[1],
            "incomplete optional service qualification",
        )?;
        if !present[0] {
            return Ok(None);
        }
    }
    let Some(candidate) = qualify_receivers(root, runner, sdk, qualify, None, qualifier)? else {
        return Ok(None);
    };
    let shells = SHELLS
        .into_iter()
        .filter(|name| under(root, name).exists())
        .collect::<Vec<_>>();
    let [shell] = shells.as_slice() else {
        return Err(invalid("expected one canonical receiving Shell"));
    };
    let proof = qualify_receiving_route(root, sdk, &candidate, shell, route_qualifier)?;
    // Route qualification executes the actual Shell; revalidate the signed
    // Browser/CEF/SDK and exact native qualifier again after it returns.
    require(
        qualify_receivers(root, runner, sdk, false, Some(&candidate), qualifier)?.as_ref()
            == Some(&candidate),
        "receiving consumers changed during route qualification",
    )?;
    Ok(Some(proof))
}

fn route_proof(candidate: Value, shell_path: &str, facts: Map<String, Value>) -> Value {
    let mut proof = candidate;
    insert(&mut proof, "schema", SERVICE_SCHEMA);
    insert(&mut proof, "receiving_shell_path", shell_path);
    if let Value::Object(fields) = &mut proof {
        fields.extend(facts);
    }
    proof
}

/// Candidate artifact qualification; not sufficient for service admission.
#[allow(
    clippy::too_many_lines,
    reason = "one linear contract: each step gates the next"
)]
fn qualify_receivers(
    root: &Path,
    runner: &mut Runner<'_>,
    sdk: &Sdk,
    qualify: bool,
    evidence: Option<&Value>,
    qualifier: &mut Qualifier<'_>,
) -> std::io::Result<Option<Value>> {
    // No claim is made on an old account-only image. A saved claim may never
    // disappear merely because one of its receiving artifacts went missing.
    if !qualify && evidence.is_none() {
        return Ok(None);
    }
    let Some(receiving) = selected(root)? else {
        require(
            evidence.is_none(),
            "saved service consumer is no longer installed",
        )?;
        return Ok(None);
    };
    require(
        receiving.runtime.as_deref()
            == Some(format!("tilo-runtime/{}", sdk.runtime_version).as_str()),
        "Browser does not receive the selected SDK runtime",
    )?;
    let publisher = read_trimmed(&root.join("usr/share/tilo/browser-runtime-publisher.hex"))?;
    require(hex_digest(&publisher), "invalid receiving CEF publisher")?;
    let env = account::env();
    let bundle = &receiving.bundle;
    account::verify_bundle(root, bundle, &publisher, runner, &env)?;
    let manifest = load_toml(&bundle.join("bundle.toml"))?;
    let cef_root = manifest
        .get("root_hash")
        .and_then(toml::Value::as_str)
        .filter(|hash| hex_digest(hash))
        .map(str::to_owned);
    require(
        manifest.get("version").and_then(toml::Value::as_str) == Some(receiving.version.as_str())
            && cef_root.is_some(),
        "verified receiving CEF identity differs",
    )?;
    let names = manifest
        .get("files")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| invalid("CEF host is absent from signed receiving inventory"))?
        .iter()
        .map(|item| {
            item.get("path")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| invalid("CEF host is absent from signed receiving inventory"))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    require(
        names.iter().collect::<BTreeSet<_>>().len() == names.len()
            && names.contains(&"lib/chromium/tilo-chromium-host"),
        "CEF host is absent from signed receiving inventory",
    )?;
    let host = bundle.join("lib/chromium/tilo-chromium-host");
    let script = under(root, QUALIFICATION_SCRIPT);
    for path in [&receiving.browser, &host, &script] {
        executable(
            path,
            RECEIVER_BOUND,
            "receiving service artifact is not executable",
        )?;
    }
    // The canonical verifier chooses this exact installed image row, never a
    // same-id package from an importing user's writable store.
    let mut package_env = env.clone();
    package_env.push((
        "TILO_APPS_DATA_DIR",
        root.join("usr/share/tilo-apps").into(),
    ));
    let verify_browser: [OsString; 3] = [
        root.join("usr/bin/tilo-install").into(),
        "verify".into(),
        "so.tilo.browser".into(),
    ];
    runner(&verify_browser, &package_env)?;
    let primary = root.join("usr/lib/tilo-runtime").join(&sdk.runtime_version);
    let probe = primary.join("bin/tilo-runtime-account-capability");
    let loader = primary.join(interpreter(&probe)?);
    let through_loader = |subject: &Path| -> Vec<OsString> {
        vec![
            loader.clone().into(),
            "--inhibit-cache".into(),
            "--library-path".into(),
            primary.join("lib").into(),
            subject.into(),
            "--service-session-capability".into(),
        ]
    };
    let probed = (|| {
        let protocol = response(&runner(&through_loader(&probe), &env)?, false)?;
        let browser = response(&runner(&through_loader(&receiving.browser), &env)?, true)?;
        require(protocol == browser, "receiving service protocols differ")?;
        Ok(protocol)
    })();
    let protocol = match probed {
        Ok(protocol) => protocol,
        // Old receivers may retain protected account grants without asserting
        // service support. A previously qualified receiver cannot downgrade.
        Err(error) if invoke::is_command_failure(&error) => {
            require(
                qualify && evidence.is_none(),
                "saved service receiver no longer supports its protocol",
            )?;
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let script_sha = digest(&script, MIB)?;
    let mut proof = json!({
        "schema": 1, "protocol": protocol,
        "sdk_runtime_root_hash": sdk.runtime_root_hash,
        "browser_package_hash": receiving.package_id_hex,
        "browser_publisher": receiving.publisher_key_hex,
        "browser_binary_sha256": digest(&receiving.browser, RECEIVER_BOUND)?,
        "cef_runtime_version": receiving.version,
        "cef_runtime_root_hash": cef_root,
        "cef_runtime_publisher": publisher,
        "cef_host_sha256": digest(&host, RECEIVER_BOUND)?,
        "qualification_script_sha256": script_sha,
    });
    if protocol == 2 {
        insert(
            &mut proof,
            "origin_storage_qualification_sha256",
            script_sha,
        );
    }
    if qualify {
        let mut command: Vec<OsString> = vec![
            "/usr/bin/python3".into(),
            script.clone().into(),
            host.clone().into(),
        ];
        if protocol == 2 {
            command.push("--capability-receipt".into());
            native_origin_completion(&qualifier(&command, &env)?)?;
        } else {
            require(
                qualifier(&command, &env)? == b"selected CEF session import: PASS\n",
                "native CEF Set/Flush/restart qualification did not complete",
            )?;
        }
    } else {
        require(
            evidence == Some(&proof),
            "installed service capability differs from qualified consumers",
        )?;
    }
    // Neither active probe nor native qualification may replace the receiving
    // artifacts before the image records their capability.
    account::verify_bundle(root, bundle, &publisher, runner, &env)?;
    runner(&verify_browser, &package_env)?;
    let share = root.join("usr/share/tilo");
    let markers = [
        share.join("account-consumer-runtime-version"),
        share.join("account-consumer-runtime-publisher.hex"),
    ];
    let present = markers.each_ref().map(|path| path.exists());
    require(
        present[0] == present[1],
        "incomplete account runtime selection",
    )?;
    if present[0] {
        require(
            read_trimmed(&markers[0])? == sdk.runtime_version,
            "service proof differs from selected account runtime",
        )?;
    }
    let base_publisher = share.join("runtime-bundle-publisher.hex");
    let sdk_publisher = read_trimmed(if present[0] {
        &markers[1]
    } else {
        &base_publisher
    })?;
    require(hex_digest(&sdk_publisher), "invalid selected SDK publisher")?;
    account::verify_bundle(root, &primary, &sdk_publisher, runner, &env)?;
    require(
        load_toml(&primary.join("bundle.toml"))?
            .get("root_hash")
            .and_then(toml::Value::as_str)
            == Some(sdk.runtime_root_hash.as_str())
            && digest(&probe, PROBE_BOUND)? == sdk.runtime_probe_sha256
            && digest(&primary.join(&sdk.runtime_library), LIBRARY_BOUND)?
                == sdk.runtime_library_sha256,
        "receiving SDK changed during qualification",
    )?;
    require(
        field(&proof, "browser_binary_sha256")
            == digest(&receiving.browser, RECEIVER_BOUND)?.as_str()
            && field(&proof, "cef_host_sha256") == digest(&host, RECEIVER_BOUND)?.as_str()
            && field(&proof, "qualification_script_sha256") == digest(&script, MIB)?.as_str(),
        "receiving service artifacts changed during qualification",
    )?;
    Ok(Some(proof))
}

/// Run the fixed image-controlled bridge of the existing owned receiving
/// coordinator. It checks the owned candidate, evidence and outer namespace
/// witness itself before starting the coordinator.
pub fn native_route_qualify(
    command: &[OsString],
    env: &[(&str, OsString)],
    request: &[u8],
) -> std::io::Result<Vec<u8>> {
    require(
        json::strict(request)?.get("root").and_then(Value::as_str) == Some("/"),
        "service qualification requires the actual owned receiving image",
    )?;
    require(
        rustix::process::getuid().is_root() && rustix::process::geteuid().is_root(),
        "service route qualification requires the canonical owned receiving coordinator",
    )?;
    for edge in Path::new(BRIDGE).ancestors() {
        let info = std::fs::symlink_metadata(edge)?;
        require(
            info.uid() == 0 && info.mode() & 0o022 == 0 && !info.file_type().is_symlink(),
            "service bridge is not image controlled",
        )?;
    }
    let mut bridged: Vec<OsString> = ["/usr/bin/python3", "-I", BRIDGE, "--owned-bridge"]
        .map(OsString::from)
        .to_vec();
    bridged.extend_from_slice(command);
    invoke::run(&invoke::Request {
        command: &bridged,
        env,
        input: Some(request),
        limit: BRIDGE_OUTPUT_BOUND,
        // The bridge bounds the coordinator at 2800 seconds itself.
        timeout: Duration::from_secs(2900),
    })
}

/// Qualify the actual admitted sandbox route.
pub(super) fn qualify_receiving_route(
    root: &Path,
    sdk: &Sdk,
    candidate: &Value,
    shell_path: &str,
    qualifier: &mut RouteQualifier<'_>,
) -> std::io::Result<Value> {
    require(
        SHELLS.contains(&shell_path),
        "receiving Shell is not a canonical packaged receiver",
    )?;
    require(
        root.is_absolute() && std::fs::canonicalize(root)? == root,
        "receiving root is not canonical",
    )?;
    let facts = route_artifacts(root, shell_path)?;
    let driver = under(root, RECEIVING_ENTRY);
    let protocol = field(candidate, "protocol").as_i64().unwrap_or_default();
    require(
        field(candidate, "sdk_runtime_root_hash") == sdk.runtime_root_hash.as_str()
            && matches!(protocol, 1 | 2),
        "route candidate differs from selected SDK",
    )?;
    let mut request = json!({
        "schema": 2, "root": root.to_str().ok_or_else(|| invalid("receiving root is not canonical"))?,
        "shell_path": shell_path,
        "sdk_runtime_version": sdk.runtime_version,
        "sdk_runtime_root_hash": sdk.runtime_root_hash,
        "browser_package_hash": field(candidate, "browser_package_hash"),
        "browser_publisher": field(candidate, "browser_publisher"),
        "cef_runtime_version": field(candidate, "cef_runtime_version"),
        "cef_runtime_root_hash": field(candidate, "cef_runtime_root_hash"),
        "cef_runtime_publisher": field(candidate, "cef_runtime_publisher"),
        "required_protocol": protocol,
    });
    if let Value::Object(fields) = &mut request {
        fields.extend(facts.clone());
    }
    let encoded = json::canonical_ascii(&request);
    require(
        encoded.len() <= 4096,
        "receiving route request exceeds bound",
    )?;
    let python = facts
        .get("receiving_python_path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let env = ENV.map(|(key, value)| (key, OsString::from(value)));
    let output = qualifier(
        &[
            under(root, python).into(),
            "-I".into(),
            driver.into(),
            "--service-session-qualification".into(),
        ],
        &env,
        &encoded,
    )?;
    require(output.len() <= 4096, "receiving route result exceeds bound")?;
    let expected = json!({
        "schema": 2, "service_session_protocol": protocol,
        "receiving_route_qualification_sha256": facts.get("receiving_route_qualification_sha256"),
        "private_list_verified": true, "private_resolve_verified": true,
        "isolated_profile_verified": true, "revocation_verified": true,
        "no_vault_mount_verified": true,
        "origin_storage_persisted": protocol == 2,
        "chat_destination_navigation_verified": true,
    });
    require(
        json::strict(&output)? == expected,
        "actual sandbox receiving route did not complete",
    )?;
    require(
        route_artifacts(root, shell_path)? == facts,
        "receiving Shell or fixed Python closure changed during qualification",
    )?;
    Ok(route_proof(candidate.clone(), shell_path, facts))
}

/// Read-only check of the image ownership of everything a saved service proof
/// names, before any of it executes.
pub fn check_trust(evidence: &Value) -> std::io::Result<()> {
    let fields = evidence
        .as_object()
        .ok_or_else(|| invalid("unsafe installed service proof identity"))?;
    let text = |name: &str| fields.get(name).and_then(Value::as_str);
    let package = text("browser_package_hash").unwrap_or_default();
    let cef = text("cef_runtime_version").unwrap_or_default();
    require(
        fields.get("schema").and_then(Value::as_i64) == Some(SERVICE_SCHEMA)
            && ROUTE_KEYS.iter().all(|key| fields.contains_key(*key))
            && hex_digest(package)
            && account::version_ok(cef),
        "unsafe installed service proof identity",
    )?;
    let shell = text("receiving_shell_path").unwrap_or_default();
    require(SHELLS.contains(&shell), "unsafe installed receiving Shell")?;
    account::image_owned(Path::new(shell))?;
    for relative in RECEIVING_DEPENDENCIES {
        account::image_owned(Path::new(relative))?;
    }
    let python = std::fs::canonicalize("/usr/bin/python3")?;
    require(
        python.to_str() == text("receiving_python_path")
            && python.parent() == Some(Path::new("/usr/bin")),
        "installed receiving Python alias changed",
    )?;
    account::image_owned(&python)?;
    for path in [
        QUALIFICATION_SCRIPT,
        "/usr/share/tilo/browser-runtime-publisher.hex",
        "/usr/share/tilo-apps",
        "/usr/share/tilo-apps/pkgs",
    ] {
        account::image_owned(Path::new(path))?;
    }
    let pkgs = Path::new("/usr/share/tilo-apps/pkgs");
    for root in [
        pkgs.join(package),
        pkgs.join(format!("{package}.pkg")),
        Path::new("/usr/lib/tilo-runtime").join(cef),
    ] {
        account::image_owned_tree(&root)?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn dropped_for_root(command: &[OsString]) -> Vec<OsString> {
    privilege_dropped(command, true)
}
