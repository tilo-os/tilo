//! The macOS `.app`: generated from `tilo-app.toml`, ad-hoc signed for
//! development, Developer ID signed and notarized for distribution.
//!
//! Distribution follows the Imager release flow and reads the same
//! environment:
//!
//! - `TILO_MAC_SIGNING_IDENTITY`: a Developer ID Application identity.
//!   Without it the package is ad-hoc signed, which `TILO_REQUIRE_SIGNING=1`
//!   refuses.
//! - `TILO_MAC_EXPECTED_TEAM_ID`: the team the signature must carry.
//! - `TILO_NOTARY_KEYCHAIN_PROFILE`, or `TILO_APPLE_ID` +
//!   `TILO_APPLE_TEAM_ID` + `TILO_APPLE_APP_PASSWORD`: notarize and staple.
//!   `TILO_REQUIRE_SIGNING=1` refuses a package it cannot notarize.
//! - `TILO_MAC_SIGN_HOOK`, `TILO_MAC_NOTARIZE_HOOK`: commands run with the
//!   `.app` (signing) or the `.zip` (notarization) path instead of the
//!   built-in step, for credential brokers.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context as _, Result, bail};
use tilo_caps::Capability;

use super::{Project, TargetPackageRequest};

/// The oldest macOS the SDK's Metal renderer supports.
const MINIMUM_SYSTEM_VERSION: &str = "13.0";

/// The runtime daemon's executable names, in the SDK's order of preference.
/// It looks for one beside the app's executable before `PATH`, so a bundle
/// carrying it in `Contents/MacOS` joins (or starts) the shared runtime
/// wherever it runs. `build --target macos` bundles the standalone runtime
/// (`tilo-runtime`), the one a stock Mac runs; `bundle-macos` keeps the name
/// of the host it is given.
pub(super) const RUNTIME_HOSTS: [&str; 2] = ["tilo-runtime", "tilo-runtime-mcp"];

/// Write `<output>/<Name>.app` around `binary` and the runtime daemon
/// `runtime_host`, with its entitlements beside it. Returns the bundle path.
pub(super) fn assemble(
    project: &Project,
    binary: &Path,
    runtime_host: &Path,
    output: &Path,
) -> Result<PathBuf> {
    let host_name = runtime_host
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| RUNTIME_HOSTS.contains(name))
        .with_context(|| {
            format!(
                "the runtime host {} is not named {}",
                runtime_host.display(),
                RUNTIME_HOSTS.join(" or ")
            )
        })?;
    let app = output.join(format!("{}.app", project.display_name()));
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    for directory in [&macos, &resources] {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
    }
    // A copy, not a link: codesign rewrites the file it signs.
    let executable = macos.join(&project.bin);
    std::fs::copy(binary, &executable)
        .with_context(|| format!("copying {} into the bundle", binary.display()))?;
    std::fs::copy(runtime_host, macos.join(host_name))
        .with_context(|| format!("copying {} into the bundle", runtime_host.display()))?;
    let icon = project.app_dir.join("assets/AppIcon.icns");
    let has_icon = icon.is_file();
    if has_icon {
        std::fs::copy(&icon, resources.join("AppIcon.icns"))
            .with_context(|| format!("copying {}", icon.display()))?;
    }
    std::fs::copy(&project.manifest_path, resources.join("tilo-app.toml"))
        .context("copying tilo-app.toml into the bundle")?;
    write_if_changed(&contents.join("Info.plist"), &info_plist(project, has_icon))?;
    write_if_changed(
        &entitlements_path(&app),
        &entitlements_plist(project, false),
    )?;
    Ok(app)
}

pub(super) fn executable(app: &Path, project: &Project) -> PathBuf {
    app.join("Contents/MacOS").join(&project.bin)
}

fn runtime_host(app: &Path) -> PathBuf {
    let macos = app.join("Contents/MacOS");
    RUNTIME_HOSTS
        .iter()
        .map(|name| macos.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| macos.join(RUNTIME_HOSTS[0]))
}

/// The entitlements file written beside `app` (codesign reads it; it is not
/// part of the bundle).
pub(super) fn entitlements_path(app: &Path) -> PathBuf {
    app.with_extension("entitlements")
}

fn write_if_changed(path: &Path, contents: &str) -> Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

fn declared(project: &Project) -> Vec<Capability> {
    let capabilities = &project.manifest.capabilities;
    let mut declared: Vec<Capability> = capabilities
        .required
        .iter()
        .chain(&capabilities.optional)
        .filter_map(|name| Capability::from_name(name))
        .collect();
    declared.sort();
    declared.dedup();
    declared
}

/// `Info.plist` from the manifest: identity, version, and a usage string for
/// every declared capability macOS prompts for.
pub(super) fn info_plist(project: &Project, has_icon: bool) -> String {
    let package = &project.manifest.package;
    let mut keys: Vec<(&str, PlistValue)> = vec![
        ("CFBundleDevelopmentRegion", PlistValue::String("en".into())),
        (
            "CFBundleDisplayName",
            PlistValue::String(project.display_name()),
        ),
        (
            "CFBundleExecutable",
            PlistValue::String(project.bin.clone()),
        ),
        ("CFBundleIdentifier", PlistValue::String(package.id.clone())),
        (
            "CFBundleInfoDictionaryVersion",
            PlistValue::String("6.0".into()),
        ),
        ("CFBundleName", PlistValue::String(project.display_name())),
        ("CFBundlePackageType", PlistValue::String("APPL".into())),
        (
            "CFBundleShortVersionString",
            PlistValue::String(package.version.clone()),
        ),
        (
            "CFBundleVersion",
            PlistValue::String(package.version.clone()),
        ),
        (
            "LSMinimumSystemVersion",
            PlistValue::String(MINIMUM_SYSTEM_VERSION.into()),
        ),
        ("NSHighResolutionCapable", PlistValue::True),
        ("NSSupportsAutomaticGraphicsSwitching", PlistValue::True),
    ];
    if has_icon {
        keys.push(("CFBundleIconFile", PlistValue::String("AppIcon".into())));
    }
    if let Some(category) = package.category.as_deref().and_then(app_store_category) {
        keys.push((
            "LSApplicationCategoryType",
            PlistValue::String(category.into()),
        ));
    }
    for capability in declared(project) {
        if let Some((key, description)) = capability.macos_usage_description() {
            keys.push((key, PlistValue::String(description.into())));
        }
    }
    keys.sort_by_key(|(key, _)| *key);
    plist(&keys)
}

/// Entitlements from the declared capabilities. The hardened runtime is
/// always on for distribution; `sandbox` adds the App Sandbox.
pub(super) fn entitlements_plist(project: &Project, sandbox: bool) -> String {
    let mut keys: Vec<&str> = declared(project)
        .into_iter()
        .flat_map(Capability::macos_entitlements)
        .copied()
        .collect();
    if sandbox {
        keys.push("com.apple.security.app-sandbox");
    }
    keys.sort_unstable();
    keys.dedup();
    let entries: Vec<(&str, PlistValue)> = keys
        .into_iter()
        .map(|key| (key, PlistValue::True))
        .collect();
    plist(&entries)
}

fn app_store_category(category: &str) -> Option<&'static str> {
    Some(match category {
        "Office" => "public.app-category.productivity",
        "Development" => "public.app-category.developer-tools",
        "Graphics" => "public.app-category.graphics-design",
        "Multimedia" | "AudioVideo" => "public.app-category.entertainment",
        "Education" => "public.app-category.education",
        "Game" => "public.app-category.games",
        "Network" => "public.app-category.social-networking",
        "Utility" | "System" | "Settings" => "public.app-category.utilities",
        _ => return None,
    })
}

enum PlistValue {
    String(String),
    True,
}

fn plist(entries: &[(&str, PlistValue)]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    for (key, value) in entries {
        let _ = writeln!(out, "\t<key>{}</key>", xml_escape(key));
        match value {
            PlistValue::String(text) => {
                let _ = writeln!(out, "\t<string>{}</string>", xml_escape(text));
            }
            PlistValue::True => out.push_str("\t<true/>\n"),
        }
    }
    out.push_str("</dict>\n</plist>\n");
    out
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Ad-hoc sign `app` so notifications and the Keychain behave as they do in
/// a release build. The runtime daemon is signed first: a bundle's seal
/// covers the nested code, which must already carry its own signature.
pub(super) fn sign_ad_hoc(app: &Path, entitlements: &Path) -> Result<()> {
    run(
        ProcessCommand::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(runtime_host(app)),
        "codesign (ad-hoc) of the runtime daemon",
    )?;
    run(
        ProcessCommand::new("codesign")
            .arg("--force")
            .arg("--sign")
            .arg("-")
            .arg("--entitlements")
            .arg(entitlements)
            .arg(app),
        "codesign (ad-hoc)",
    )
}

fn run(command: &mut ProcessCommand, what: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("starting {what}"))?;
    if !status.success() {
        bail!("{what} failed with {status}");
    }
    Ok(())
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

enum Notary {
    KeychainProfile(String),
    AppleId {
        apple_id: String,
        team_id: String,
        password: String,
    },
    Hook(String),
}

fn notary() -> Option<Notary> {
    if let Some(hook) = env("TILO_MAC_NOTARIZE_HOOK") {
        return Some(Notary::Hook(hook));
    }
    if let Some(profile) = env("TILO_NOTARY_KEYCHAIN_PROFILE") {
        return Some(Notary::KeychainProfile(profile));
    }
    Some(Notary::AppleId {
        apple_id: env("TILO_APPLE_ID")?,
        team_id: env("TILO_APPLE_TEAM_ID")?,
        password: env("TILO_APPLE_APP_PASSWORD")?,
    })
}

/// Sign `app` for distribution, verify it, zip it and, when configured,
/// notarize and staple. Returns the zip.
pub(super) fn package(
    project: &Project,
    app: &Path,
    request: &TargetPackageRequest,
) -> Result<PathBuf> {
    let require_signing = env("TILO_REQUIRE_SIGNING").as_deref() == Some("1");
    let output = request.output.as_ref().map_or_else(
        || {
            project.app_dir.join("dist").join(format!(
                "{}-{}-macos.zip",
                project.display_name().replace(' ', "-"),
                project.manifest.package.version
            ))
        },
        |path| crate::absolute_from(&project.app_dir, path),
    );
    if output.exists() && !request.force {
        bail!(
            "{} already exists; pass --force to replace it",
            output.display()
        );
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    sign_for_distribution(project, app, require_signing)?;
    zip(app, &output)?;
    match notary() {
        Some(notary) => {
            notarize(&notary, &output)?;
            for step in ["staple", "validate"] {
                run(
                    ProcessCommand::new("xcrun")
                        .args(["stapler", step])
                        .arg(app),
                    "stapler",
                )?;
            }
            zip(app, &output)?;
        }
        None if require_signing => bail!(
            "TILO_REQUIRE_SIGNING=1 but notarization is not configured\n  help: set TILO_NOTARY_KEYCHAIN_PROFILE, or TILO_APPLE_ID + TILO_APPLE_TEAM_ID + TILO_APPLE_APP_PASSWORD"
        ),
        None => {}
    }
    let descriptor = descriptor_path(&output);
    std::fs::write(&descriptor, signing_descriptor(project, app)?)
        .with_context(|| format!("writing {}", descriptor.display()))?;
    eprintln!(
        "[tilo-app] packaged {} (signing descriptor {})",
        output.display(),
        descriptor.display()
    );
    Ok(output)
}

/// `<zip>.json`: what `tilo-pkg entry --platform macos` reads beside the zip.
pub(super) fn descriptor_path(zip: &Path) -> PathBuf {
    let mut name = zip.as_os_str().to_owned();
    name.push(".json");
    PathBuf::from(name)
}

/// The registry artifact descriptor for the signed `app`: the bundle's
/// names, and the signature facts only `codesign` can read (ad hoc or
/// Developer ID, the team, the code-directory hash), plus whether a
/// notarization ticket is stapled. Its shape is the registry's
/// `macos-app-zip` artifact.
fn signing_descriptor(project: &Project, app: &Path) -> Result<String> {
    let details = ProcessCommand::new("codesign")
        .arg("-dvvv")
        .arg(app)
        .output()
        .context("reading the signature")?;
    if !details.status.success() {
        bail!("codesign -dvvv failed with {}", details.status);
    }
    let (signing, team_id, cdhash) = signature_facts(&String::from_utf8_lossy(&details.stderr))?;
    let bundle = app
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .context("the bundle has no name")?;
    let descriptor = serde_json::json!({
        "format": "macos-app-zip",
        "bundle": bundle,
        "executable": project.bin,
        "minimum_system_version": MINIMUM_SYSTEM_VERSION,
        "signing": signing,
        "team_id": team_id,
        "notarized": app.join("Contents/CodeResources").is_file(),
        "cdhash_hex": cdhash,
    });
    Ok(serde_json::to_string_pretty(&descriptor)? + "\n")
}

/// `(signing, team, cdhash)` from `codesign -dvvv`'s report.
fn signature_facts(report: &str) -> Result<(&'static str, Option<String>, String)> {
    let field = |name: &str| {
        report
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
            .map(str::trim)
    };
    let cdhash = field("CDHash")
        .filter(|hash| !hash.is_empty() && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .context("codesign reports no CDHash")?
        .to_ascii_lowercase();
    let team = field("TeamIdentifier")
        .filter(|team| *team != "not set")
        .map(str::to_owned);
    if field("Signature") == Some("adhoc") {
        return Ok(("ad-hoc", None, cdhash));
    }
    let team = team.context("a Developer ID signature carries a TeamIdentifier")?;
    Ok(("developer-id", Some(team), cdhash))
}

/// Developer ID (or hook, or ad-hoc) signing with the hardened runtime,
/// then a strict verification and the expected team check.
fn sign_for_distribution(project: &Project, app: &Path, require_signing: bool) -> Result<()> {
    let entitlements = entitlements_path(app);
    std::fs::write(&entitlements, entitlements_plist(project, false))
        .with_context(|| format!("writing {}", entitlements.display()))?;
    let binary = executable(app, project);
    if let Some(hook) = env("TILO_MAC_SIGN_HOOK") {
        run(ProcessCommand::new(&hook).arg(app), "TILO_MAC_SIGN_HOOK")?;
    } else if let Some(identity) = env("TILO_MAC_SIGNING_IDENTITY") {
        // The daemon holds no app capability, so it gets none of the app's
        // entitlements.
        run(
            ProcessCommand::new("codesign")
                .args(["--force", "--options", "runtime", "--timestamp"])
                .arg("--sign")
                .arg(&identity)
                .arg(runtime_host(app)),
            "codesign of the runtime daemon",
        )?;
        for path in [binary.as_path(), app] {
            run(
                ProcessCommand::new("codesign")
                    .args(["--force", "--options", "runtime", "--timestamp"])
                    .arg("--entitlements")
                    .arg(&entitlements)
                    .arg("--sign")
                    .arg(&identity)
                    .arg(path),
                "codesign",
            )?;
        }
    } else if require_signing {
        bail!(
            "TILO_REQUIRE_SIGNING=1 but no signing identity is configured\n  help: set TILO_MAC_SIGNING_IDENTITY to a Developer ID Application identity, or TILO_MAC_SIGN_HOOK"
        );
    } else {
        eprintln!(
            "[tilo-app] no TILO_MAC_SIGNING_IDENTITY; ad-hoc signing (runs on this Mac only)"
        );
        sign_ad_hoc(app, &entitlements)?;
    }
    run(
        ProcessCommand::new("codesign")
            .args(["--verify", "--deep", "--strict", "--verbose=2"])
            .arg(app),
        "codesign --verify",
    )?;
    if let Some(expected) = env("TILO_MAC_EXPECTED_TEAM_ID") {
        let details = ProcessCommand::new("codesign")
            .arg("-dvv")
            .arg(app)
            .output()
            .context("reading the signature")?;
        let text = String::from_utf8_lossy(&details.stderr);
        let team = text
            .lines()
            .find_map(|line| line.strip_prefix("TeamIdentifier="))
            .unwrap_or("");
        if team != expected {
            bail!("signed by team {team:?}, expected TILO_MAC_EXPECTED_TEAM_ID={expected:?}");
        }
    }
    Ok(())
}

fn notarize(notary: &Notary, zip: &Path) -> Result<()> {
    match notary {
        Notary::Hook(hook) => run(ProcessCommand::new(hook).arg(zip), "TILO_MAC_NOTARIZE_HOOK"),
        Notary::KeychainProfile(profile) => run(
            ProcessCommand::new("xcrun")
                .args(["notarytool", "submit"])
                .arg(zip)
                .args(["--wait", "--keychain-profile", profile]),
            "notarytool submit",
        ),
        Notary::AppleId {
            apple_id,
            team_id,
            password,
        } => run(
            ProcessCommand::new("xcrun")
                .args(["notarytool", "submit"])
                .arg(zip)
                .args([
                    "--wait",
                    "--apple-id",
                    apple_id,
                    "--team-id",
                    team_id,
                    "--password",
                    password,
                ]),
            "notarytool submit",
        ),
    }
}

fn zip(app: &Path, output: &Path) -> Result<()> {
    if output.exists() {
        std::fs::remove_file(output).with_context(|| format!("replacing {}", output.display()))?;
    }
    run(
        ProcessCommand::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(app)
            .arg(output),
        "ditto",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(capabilities: &str) -> Project {
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            "[package]\nid = \"com.example.hello\"\nname = \"Hello & Co\"\ncategory = \"Office\"\nversion = \"1.2.3\"\nkind = \"app\"\n\n[publisher]\nidentity = \"ed25519:{zero}\"\nsignature = \"\"\n\n[sandbox]\nnamespaces = [\"user\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\n[sandbox.net]\nmode = \"off\"\n\n[entrypoint]\nbin = \"bin/hello\"\ntype = \"gui\"\n\n{capabilities}",
            zero = "0".repeat(64)
        ))
        .unwrap();
        Project {
            app_dir: PathBuf::from("/app"),
            cargo_manifest: PathBuf::from("/app/Cargo.toml"),
            package: "hello".into(),
            lib_name: "hello".into(),
            bin: "hello".into(),
            manifest_path: PathBuf::from("/app/tilo-app.toml"),
            manifest,
            workspace_root: PathBuf::from("/app"),
            target_dir: PathBuf::from("/app/target"),
            sdk_root: PathBuf::from("/sdk"),
        }
    }

    #[test]
    fn unsigned_assembly_preserves_inputs_and_refuses_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let mut project = project("[capabilities]\noptional = [\"camera\"]\n");
        project.bin = "cargo-package-fallback".into();
        project.app_dir = root.path().join("project with spaces");
        project.manifest_path = project.app_dir.join("tilo-app.toml");
        std::fs::create_dir_all(project.app_dir.join("assets")).unwrap();
        std::fs::write(&project.manifest_path, "manifest fixture").unwrap();
        std::fs::write(project.app_dir.join("assets/AppIcon.icns"), b"icon").unwrap();
        let binary = root.path().join("unsigned app input");
        let daemon = root.path().join("tilo-runtime");
        std::fs::write(&binary, b"app fixture, not executable").unwrap();
        std::fs::write(&daemon, b"helper fixture, not executable").unwrap();
        let output = root.path().join("new output");
        let app =
            super::super::assemble_macos_project(&project, &binary, &daemon, &output).unwrap();
        assert_eq!(
            std::fs::read(app.join("Contents/MacOS/hello")).unwrap(),
            std::fs::read(&binary).unwrap()
        );
        assert_eq!(
            std::fs::read(runtime_host(&app)).unwrap(),
            std::fs::read(&daemon).unwrap()
        );
        assert_eq!(
            std::fs::read(app.join("Contents/Resources/AppIcon.icns")).unwrap(),
            b"icon"
        );
        let info = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
        assert!(info.contains("<string>13.0</string>"));
        assert!(info.contains("<key>CFBundleExecutable</key>\n\t<string>hello</string>"));
        assert!(!app.join("Contents/MacOS/cargo-package-fallback").exists());
        assert!(info.contains("NSCameraUsageDescription"));
        assert_eq!(
            std::fs::read_to_string(entitlements_path(&app)).unwrap(),
            entitlements_plist(&project, false)
        );
        assert!(super::super::assemble_macos_project(&project, &binary, &daemon, &output).is_err());
        assert_eq!(
            std::fs::read(&binary).unwrap(),
            b"app fixture, not executable"
        );
        project.manifest.entrypoint.as_mut().unwrap().bin = PathBuf::from("bin/TILO-RUNTIME-MCP");
        assert!(
            super::super::assemble_macos_project(
                &project,
                &binary,
                &daemon,
                &root.path().join("collision")
            )
            .is_err()
        );
    }

    #[test]
    fn unsigned_assembly_failure_cleans_only_its_temporary_directory() {
        let root = tempfile::tempdir().unwrap();
        let mut project = project("");
        project.manifest_path = root.path().join("missing manifest");
        let binary = root.path().join("input");
        std::fs::write(&binary, b"fixture").unwrap();
        let output = root.path().join("output");
        assert!(super::super::assemble_macos_project(&project, &binary, &binary, &output).is_err());
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        project.manifest.package.name = Some("../escape".into());
        assert!(super::super::assemble_macos_project(&project, &binary, &binary, &output).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn info_plist_comes_from_the_manifest() {
        let plist = info_plist(
            &project("[capabilities]\noptional = [\"camera\", \"network.http\"]\n"),
            false,
        );
        assert!(
            plist.contains("<key>CFBundleIdentifier</key>\n\t<string>com.example.hello</string>")
        );
        assert!(plist.contains("<key>CFBundleShortVersionString</key>\n\t<string>1.2.3</string>"));
        assert!(plist.contains("<string>Hello &amp; Co</string>"));
        assert!(plist.contains("<key>NSCameraUsageDescription</key>"));
        assert!(plist.contains("public.app-category.productivity"));
        assert!(!plist.contains("CFBundleIconFile"));
    }

    #[test]
    fn the_bundle_carries_the_runtime_daemon_beside_the_app() {
        let root = tempfile::tempdir().unwrap();
        let mut project = project("");
        project.app_dir = root.path().join("app");
        project.manifest_path = project.app_dir.join("tilo-app.toml");
        std::fs::create_dir_all(&project.app_dir).unwrap();
        std::fs::write(&project.manifest_path, "").unwrap();
        let binary = root.path().join("hello");
        let daemon = root.path().join("tilo-runtime");
        std::fs::write(&binary, "app").unwrap();
        std::fs::write(&daemon, "daemon").unwrap();
        let app = assemble(&project, &binary, &daemon, &root.path().join("out")).unwrap();
        assert_eq!(
            std::fs::read_to_string(executable(&app, &project)).unwrap(),
            "app"
        );
        assert_eq!(
            std::fs::read_to_string(executable(&app, &project).with_file_name("tilo-runtime"))
                .unwrap(),
            "daemon",
            "the SDK finds the daemon beside the executable"
        );
    }

    #[test]
    fn the_descriptor_reads_codesigns_report() {
        let ad_hoc = "CDHash=EEFBAEBFAD82DD17d1399cf9a48918a68955dc6a\nSignature=adhoc\nTeamIdentifier=not set\n";
        assert_eq!(
            signature_facts(ad_hoc).unwrap(),
            (
                "ad-hoc",
                None,
                "eefbaebfad82dd17d1399cf9a48918a68955dc6a".to_owned()
            )
        );
        let developer = "Authority=Developer ID Application: Tilo (ABCDE12345)\nCDHash=0123456789abcdef0123456789abcdef01234567\nTeamIdentifier=ABCDE12345\n";
        assert_eq!(
            signature_facts(developer).unwrap().1.as_deref(),
            Some("ABCDE12345")
        );
        assert!(signature_facts("Signature=adhoc\n").is_err());
        assert_eq!(
            descriptor_path(Path::new("/d/Hello-1.0-macos.zip")),
            Path::new("/d/Hello-1.0-macos.zip.json")
        );
    }

    #[test]
    fn entitlements_follow_the_capabilities() {
        let none = entitlements_plist(&project(""), false);
        assert!(none.contains("<dict>\n</dict>"), "{none}");
        let some = entitlements_plist(
            &project(
                "[capabilities]\nrequired = [\"network.http\"]\noptional = [\"files.save\", \"files.open\"]\n",
            ),
            true,
        );
        assert!(some.contains("com.apple.security.network.client"));
        assert_eq!(
            some.matches("com.apple.security.files.user-selected.read-write")
                .count(),
            1
        );
        assert!(some.contains("com.apple.security.app-sandbox"));
    }
}
