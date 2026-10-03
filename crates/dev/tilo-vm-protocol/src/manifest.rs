//! Reader for `os/components.toml`, the component manifest.
//!
//! The manifest belongs to the image build, and `os/scripts/install-components.py`
//! owns every path decision in it; dev tooling reads only what it needs to
//! build a component and to decide what a swap restarts:
//!
//! ```toml
//! [[component]]
//! name = "fs"
//! kind = "userspace"          # or "boot", with `scenario` naming the os/dev driver
//! dev_swap = "reboot"         # boot components tilo-vm can swap (the kernel)
//! dev_build = "os/kernel/dev-build.sh"  # and the script that builds them
//! scope = "vm"                # "session" | "vm"; anything else reads as session
//! profile = "release"         # cargo profile, default release
//! packages = ["fs"]
//! features = ["fs/foo"]       # package-qualified
//! dev_features = ["fs/dev-overrides"]  # dev builds only
//! units = ["fs.service"]      # system units a vm-scope swap restarts
//! user_units = ["tilo-fs-recipient.service"]
//! files = [{ from = "fs", to = "/usr/bin/fs", mode = "0755" }]
//! ```
//!
//! Everything else is ignored, so the schema can grow without breaking dev
//! tools, and malformed entries are skipped rather than fatal.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use toml::Value;

/// What a swap of this component restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Compositor, user services, CLIs a user runs: restart the slot's session.
    Session,
    /// System services, greeter: merge into the VM's `/usr`.
    Vm,
    /// Apps alone: live at next launch, no restart.
    App,
}

impl Scope {
    fn parse(text: &str) -> Self {
        match text {
            "vm" => Self::Vm,
            "app" | "apps" => Self::App,
            // Restarting the session is the safe default for anything unknown.
            _ => Self::Session,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Vm => "vm",
            Self::App => "app",
        }
    }
}

/// The part of a component the daemon needs to apply a swap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentInfo {
    pub scope: Scope,
    #[serde(default)]
    pub units: Vec<String>,
    /// Takes effect at the next boot (`dev_swap = "reboot"`, the kernel):
    /// the daemon installs it as the VM's next boot entry and the client
    /// reboots the VM.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reboot: bool,
}

impl ComponentInfo {
    /// A component of `scope` that restarts `units` and needs no reboot.
    pub fn new(scope: Scope, units: Vec<String>) -> Self {
        Self {
            scope,
            units,
            reboot: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    /// `kind = "boot"`: only a cold boot exercises it. tilo-vm swaps it only
    /// when it also has `dev_swap` (then `info.reboot` is set); otherwise a
    /// swap is redirected to `scenario`.
    pub boot: bool,
    /// For boot components: the os/dev driver that cold-boots them.
    pub scenario: Option<String>,
    /// For swappable boot components: the script that builds them for a dev
    /// VM, relative to the repository root.
    pub dev_build: Option<String>,
    pub info: ComponentInfo,
    pub user_units: Vec<String>,
    pub packages: Vec<String>,
    /// Only the manifest's installed binary targets, rather than every binary
    /// in each package (which can include heavyweight diagnostic tools).
    pub bins: Vec<String>,
    /// Package-qualified cargo features (`pkg/feature`).
    pub features: Vec<String>,
    /// Features only dev builds enable, normally package-qualified. A bare
    /// feature can target a single package with a shadowing self dependency.
    pub dev_features: Vec<String>,
    pub profile: String,
    /// Absolute install paths (`to`) of the component's files; a trailing
    /// `/` marks a whole directory tree. What a CI layer extracts.
    pub paths: Vec<String>,
    /// `layer = "direct"`: the image build writes these files itself (the
    /// signed runtime bundle, the thin-app store), so no cargo build of
    /// `packages` alone produces them.
    pub image_assembled: bool,
}

/// Is `profile` a cargo profile name tilo-vm will pass on?
pub fn valid_profile(profile: &str) -> bool {
    !profile.is_empty()
        && profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Component {
    /// A boot component tilo-vm cannot swap: only its scenario exercises it.
    pub fn scenario_only(&self) -> bool {
        self.boot && !self.info.reboot
    }

    /// This component built with another cargo profile (`swap --profile`).
    #[must_use]
    pub fn with_profile(mut self, profile: Option<&str>) -> Self {
        if let Some(profile) = profile {
            profile.clone_into(&mut self.profile);
        }
        self
    }

    /// The directory under the target dir that cargo writes this profile
    /// to: `dev` and `test` land in `debug`, `bench` in `release`.
    pub fn profile_dir(&self) -> &str {
        match self.profile.as_str() {
            "dev" | "test" => "debug",
            "bench" => "release",
            other => other,
        }
    }

    /// Every feature a dev build enables: the manifest's, its dev features,
    /// then `extra` (e.g. `pkg/dev-overrides` found in `cargo metadata`).
    pub fn build_features(&self, extra: &[String]) -> Vec<String> {
        let mut features: Vec<String> = Vec::new();
        for feature in self.features.iter().chain(&self.dev_features).chain(extra) {
            if !features.contains(feature) {
                features.push(feature.clone());
            }
        }
        features
    }

    /// `cargo` arguments (after the program name) that build this
    /// component for a dev layer. Both builders run exactly these.
    pub fn cargo_args(&self, extra: &[String]) -> Vec<String> {
        let mut args: Vec<String> = ["build", "--locked", "--profile", &self.profile]
            .map(str::to_owned)
            .to_vec();
        for package in &self.packages {
            args.push("-p".into());
            args.push(package.clone());
        }
        for bin in &self.bins {
            args.push("--bin".into());
            args.push(bin.clone());
        }
        let features = self.build_features(extra);
        if !features.is_empty() {
            args.push("--features".into());
            args.push(features.join(","));
        }
        args
    }

    /// Arguments for `python3` that lay the build out under `dest` with the
    /// manifest installer, which owns every path decision.
    pub fn installer_args(&self, from: &str, dest: &str) -> Vec<String> {
        [
            INSTALLER, "--quiet", "--from", from, "--dest", dest, &self.name,
        ]
        .map(str::to_owned)
        .to_vec()
    }
}

/// The manifest installer, relative to the repository root.
pub const INSTALLER: &str = "os/scripts/install-components.py";

/// The cargo feature that turns on developer overrides in a package.
pub const DEV_FEATURE: &str = "dev-overrides";

/// `pkg/dev-overrides` for each of `packages` that declares the feature,
/// read from `cargo metadata --no-deps --format-version 1` output.
pub fn dev_override_features(metadata: &serde_json::Value, packages: &[String]) -> Vec<String> {
    let Some(all) = metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    packages
        .iter()
        .filter(|name| {
            all.iter().any(|p| {
                p.get("name").and_then(serde_json::Value::as_str) == Some(name.as_str())
                    && p.get("features").and_then(|f| f.get(DEV_FEATURE)).is_some()
            })
        })
        .map(|name| format!("{name}/{DEV_FEATURE}"))
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub components: BTreeMap<String, Component>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("{path} is not valid TOML: {detail}")]
    Parse { path: String, detail: String },
}

impl Manifest {
    /// Load the manifest, or `Ok(None)` when the file does not exist.
    ///
    /// # Errors
    /// Unreadable or syntactically invalid files.
    pub fn load(path: &Path) -> Result<Option<Self>, ManifestError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text)
                .map(Some)
                .map_err(|detail| ManifestError::Parse {
                    path: path.display().to_string(),
                    detail,
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ManifestError::Read {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Parse manifest text: `[[component]]` entries (a `[components.<name>]`
    /// table is accepted too).
    ///
    /// # Errors
    /// Invalid TOML. Entries lacking a name are skipped, not rejected.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let mut components = BTreeMap::new();
        for key in ["component", "components"] {
            match root.get(key) {
                Some(Value::Array(entries)) => {
                    for entry in entries {
                        if let Value::Table(entry) = entry
                            && let Some(Value::String(name)) = entry.get("name")
                        {
                            components.insert(name.clone(), component(name, entry));
                        }
                    }
                }
                Some(Value::Table(table)) => {
                    for (name, entry) in table {
                        if let Value::Table(entry) = entry {
                            components.insert(name.clone(), component(name, entry));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(Self { components })
    }

    pub fn get(&self, name: &str) -> Option<&Component> {
        self.components.get(name)
    }
}

fn component(name: &str, entry: &toml::Table) -> Component {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
    let boot = text("kind").as_deref() == Some("boot");
    let dev_build = text("dev_build").filter(|_| boot);
    Component {
        name: name.to_owned(),
        boot,
        scenario: text("scenario"),
        info: ComponentInfo {
            scope: text("scope")
                .as_deref()
                .map_or(Scope::Session, Scope::parse),
            units: strings(entry, "units"),
            reboot: boot && text("dev_swap").as_deref() == Some("reboot") && dev_build.is_some(),
        },
        dev_build,
        user_units: strings(entry, "user_units"),
        packages: strings(entry, "packages"),
        bins: strings(entry, "bins"),
        features: strings(entry, "features"),
        dev_features: strings(entry, "dev_features"),
        profile: text("profile").unwrap_or_else(|| "release".into()),
        paths: file_paths(entry),
        image_assembled: text("layer").as_deref() == Some("direct"),
    }
}

fn file_paths(entry: &toml::Table) -> Vec<String> {
    let Some(Value::Array(files)) = entry.get("files") else {
        return Vec::new();
    };
    files
        .iter()
        .filter_map(Value::as_table)
        .filter_map(|file| file.get("to").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// A string or array of strings; non-strings are dropped.
fn strings(entry: &toml::Table, key: &str) -> Vec<String> {
    match entry.get(key) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_swap_selects_only_the_installed_binary() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../os/components.toml");
        let manifest = Manifest::load(&path).unwrap().unwrap();
        let shell = manifest
            .get("shell")
            .unwrap()
            .clone()
            .with_profile(Some("dev"));
        assert_eq!(shell.bins, ["shell"]);
        assert_eq!(
            shell.cargo_args(&[]),
            [
                "build",
                "--locked",
                "--profile",
                "dev",
                "-p",
                "shell",
                "--bin",
                "shell",
                "--features",
                "dev-overrides"
            ]
        );
        let manifest = Manifest::parse(
            "[[component]]\nname = \"pair\"\npackages = [\"a\", \"b\"]\nbins = [\"a-cli\", \"b-cli\"]\n",
        ).unwrap();
        assert_eq!(
            manifest.get("pair").unwrap().cargo_args(&[]).join(" "),
            "build --locked --profile release -p a -p b --bin a-cli --bin b-cli"
        );
    }

    /// The component array form parses; reboot travels only when set; a profile override changes the build
    /// and its output dir; cargo and installer arguments; unknown scope is session; the repository manifest
    /// parses; a missing file is none.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn component_manifests_parse_every_form_and_default() {
        // Phase: component_array_form.
        {
            let m = Manifest::parse(
                r#"
                external_units = ["greetd.service"]
                [[component]]
                name = "fs"
                kind = "userspace"
                scope = "vm"
                packages = ["fs"]
                units = ["fs.service", "tilo-fs-replica.service"]
                user_units = ["tilo-fs-recipient.service"]
                files = [{ from = "fs", to = "/usr/bin/fs", mode = "0755" }]
                [[component]]
                name = "browser"
                scope = "session"
                packages = ["browser"]
                features = ["browser/wpe"]
                [[component]]
                name = "apps+runtime"
                scope = "session"
                profile = "thin-app"
                layer = "direct"
                [[component]]
                name = "kernel"
                kind = "boot"
                scope = "vm"
                scenario = "os/dev/vm.sh"
                dev_swap = "reboot"
                dev_build = "os/kernel/dev-build.sh"
                [[component]]
                name = "uki"
                kind = "boot"
                scope = "vm"
                scenario = "os/dev/test-measured-boot.sh"
                [[component]]
                scope = "vm"
                "#,
            )
            .unwrap();
            assert_eq!(m.components.len(), 5);
            let fs = m.get("fs").unwrap();
            assert_eq!(fs.info.scope, Scope::Vm);
            assert_eq!(fs.info.units, ["fs.service", "tilo-fs-replica.service"]);
            assert_eq!(fs.user_units, ["tilo-fs-recipient.service"]);
            assert!(!fs.boot);
            assert_eq!(fs.profile, "release");
            assert_eq!(m.get("browser").unwrap().features, ["browser/wpe"]);
            assert_eq!(fs.paths, ["/usr/bin/fs"]);
            assert_eq!(m.get("apps+runtime").unwrap().profile, "thin-app");
            assert!(m.get("apps+runtime").unwrap().image_assembled);
            assert!(!fs.image_assembled);
            let kernel = m.get("kernel").unwrap();
            assert!(kernel.boot);
            assert_eq!(kernel.scenario.as_deref(), Some("os/dev/vm.sh"));
            assert!(kernel.info.reboot && !kernel.scenario_only());
            assert_eq!(kernel.dev_build.as_deref(), Some("os/kernel/dev-build.sh"));
            let uki = m.get("uki").unwrap();
            assert!(uki.scenario_only() && !uki.info.reboot && uki.dev_build.is_none());
            assert!(!fs.scenario_only());
        }

        // Phase: reboot_travels_only_when_set.
        {
            let plain = serde_json::to_string(&ComponentInfo::new(Scope::Vm, vec![])).unwrap();
            assert_eq!(plain, r#"{"scope":"vm","units":[]}"#);
            let old: ComponentInfo = serde_json::from_str(&plain).unwrap();
            assert!(!old.reboot);
            let kernel = ComponentInfo {
                reboot: true,
                ..ComponentInfo::new(Scope::Vm, vec![])
            };
            assert!(
                serde_json::to_string(&kernel)
                    .unwrap()
                    .contains(r#""reboot":true"#)
            );
        }

        // Phase: a_profile_override_changes_the_build_and_its_output_dir.
        {
            let m = Manifest::parse("[[component]]\nname = \"shell\"\npackages = [\"shell\"]\n")
                .unwrap();
            let shell = m.get("shell").unwrap().clone();
            assert_eq!(shell.profile_dir(), "release");
            let dev = shell.clone().with_profile(Some("dev"));
            assert_eq!(
                dev.cargo_args(&[])[..4],
                ["build", "--locked", "--profile", "dev"]
            );
            assert_eq!(dev.profile_dir(), "debug");
            assert_eq!(shell.clone().with_profile(None).profile, "release");
            assert_eq!(
                shell.with_profile(Some("thin-app")).profile_dir(),
                "thin-app"
            );
            assert!(valid_profile("dev"));
            assert!(!valid_profile("../x"));
            assert!(!valid_profile(""));
        }

        // Phase: cargo_and_installer_arguments.
        {
            let m = Manifest::parse(
                "[[component]]\nname = \"browser\"\npackages = [\"browser\", \"helper\"]\nfeatures = [\"browser/wpe\"]\ndev_features = [\"browser/dev-overrides\"]\n",
            )
            .unwrap();
            let browser = m.get("browser").unwrap();
            let extra = dev_override_features(
                &serde_json::json!({"packages": [
                    {"name": "browser", "features": {"wpe": [], "dev-overrides": []}},
                    {"name": "helper", "features": {"dev-overrides": []}},
                ]}),
                &browser.packages,
            );
            assert_eq!(extra, ["browser/dev-overrides", "helper/dev-overrides"]);
            assert_eq!(
                browser.cargo_args(&extra).join(" "),
                "build --locked --profile release -p browser -p helper --features browser/wpe,browser/dev-overrides,helper/dev-overrides"
            );
            assert_eq!(
                browser
                    .installer_args("target/release", "/l/tree")
                    .join(" "),
                "os/scripts/install-components.py --quiet --from target/release --dest /l/tree browser"
            );
        }

        // Phase: bare_dev_feature_reaches_a_single_package_even_with_a_self_dependency.
        {
            let m = Manifest::parse(
                "[[component]]\nname = \"shell\"\npackages = [\"shell\"]\ndev_features = [\"dev-overrides\"]\n",
            )
            .unwrap();
            let shell = m.get("shell").unwrap();
            let extra = dev_override_features(
                &serde_json::json!({"packages": [
                    {"name": "shell", "features": {"dev-overrides": []}},
                ]}),
                &shell.packages,
            );
            assert_eq!(
                shell.cargo_args(&extra).join(" "),
                "build --locked --profile release -p shell --features dev-overrides,shell/dev-overrides"
            );
        }

        // Phase: unknown_scope_is_session.
        {
            let m = Manifest::parse("[[component]]\nname = \"x\"\nscope = \"something-new\"\n")
                .unwrap();
            assert_eq!(m.get("x").unwrap().info.scope, Scope::Session);
        }

        // Phase: the_repository_manifest_parses.
        {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../os/components.toml");
            let m = Manifest::load(&path)
                .unwrap()
                .expect("os/components.toml exists");
            assert_eq!(m.get("shell").unwrap().info.scope, Scope::Session);
            assert_eq!(m.get("fs").unwrap().info.scope, Scope::Vm);
            let kernel = m.get("kernel").unwrap();
            assert!(kernel.boot && kernel.info.reboot);
            for scenario_only in ["installer", "rollback", "initramfs", "uki", "bootloader"] {
                assert!(
                    m.get(scenario_only).unwrap().scenario_only(),
                    "{scenario_only} must stay scenario-only"
                );
            }
        }

        // Phase: missing_file_is_none.
        {
            assert!(
                Manifest::load(Path::new("/nonexistent/components.toml"))
                    .unwrap()
                    .is_none()
            );
        }
    }
}
