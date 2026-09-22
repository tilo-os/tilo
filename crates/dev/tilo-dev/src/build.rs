//! `swap --from worktree` on the host: build a component with plain cargo,
//! lay it out with the manifest installer, and pack the layer for upload.
//! Path decisions stay in `os/scripts/install-components.py`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tilo_dev_protocol::Provenance;
use tilo_dev_protocol::manifest::Component;

use crate::context::Worktree;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner};

pub const INSTALLER: &str = "os/scripts/install-components.py";
pub const DEV_FEATURE: &str = "dev-overrides";

/// `pkg/dev-overrides` for each of `packages` that declares the feature,
/// read from `cargo metadata`. Production images never enable it.
pub fn dev_features(metadata: &Value, packages: &[String]) -> Vec<String> {
    let Some(all) = metadata.get("packages").and_then(Value::as_array) else {
        return Vec::new();
    };
    packages
        .iter()
        .filter(|name| {
            all.iter().any(|p| {
                p.get("name").and_then(Value::as_str) == Some(name.as_str())
                    && p.get("features").and_then(|f| f.get(DEV_FEATURE)).is_some()
            })
        })
        .map(|name| format!("{name}/{DEV_FEATURE}"))
        .collect()
}

pub fn cargo_build(component: &Component, extra_features: &[String]) -> CommandLine {
    let mut command = CommandLine::new(
        "cargo",
        &["build", "--locked", "--profile", &component.profile],
    );
    for package in &component.packages {
        command = command.arg("-p").arg(package.clone());
    }
    let mut features = component.features.clone();
    features.extend(
        extra_features
            .iter()
            .filter(|f| !component.features.contains(f))
            .cloned(),
    );
    if !features.is_empty() {
        command = command.arg("--features").arg(features.join(","));
    }
    command
}

pub fn install(component: &Component, dest: &Path) -> CommandLine {
    let from = format!("target/{}", component.profile);
    CommandLine::new(
        "python3",
        &[INSTALLER, "--quiet", "--from", &from, "--dest"],
    )
    .arg(dest.display().to_string())
    .arg(component.name.clone())
}

/// Pack `dir` (holding `usr/`) as an uncompressed tar, symlinks kept.
///
/// # Errors
/// I/O failures.
pub fn pack(dir: &Path, out: &Path) -> Result<(), CliError> {
    let file =
        std::fs::File::create(out).map_err(CliError::io(format!("create {}", out.display())))?;
    let mut builder = tar::Builder::new(file);
    builder.follow_symlinks(false);
    for top in ["usr", "opt"] {
        let path = dir.join(top);
        if path.is_dir() {
            builder
                .append_dir_all(top, &path)
                .map_err(CliError::io(format!("pack {}", path.display())))?;
        }
    }
    builder
        .into_inner()
        .map(drop)
        .map_err(CliError::io("finish layer archive"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn first_line(runner: &mut dyn Runner, command: &CommandLine) -> Option<String> {
    let output = runner.output(command).ok()?;
    output.success.then(|| {
        output
            .stdout
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned()
    })
}

/// Everything that identifies a host build of `component`.
pub fn provenance(
    runner: &mut dyn Runner,
    worktree: &Worktree,
    component: &Component,
    features: Vec<String>,
) -> Provenance {
    let mut provenance = Provenance::local(&component.name);
    provenance.source_commit = worktree.head();
    provenance.dirty_tree_hash = worktree
        .dirty_state()
        .map(|state| sha256_hex(state.as_bytes()));
    provenance.cargo_profile = Some(component.profile.clone());
    provenance.features = features;
    provenance.builder = Some("host".into());
    provenance.cargo_lock_hash = std::fs::read(worktree.root.join("Cargo.lock"))
        .ok()
        .map(|b| sha256_hex(&b));
    provenance.toolchain = first_line(runner, &CommandLine::new("rustc", &["--version"]));
    if let Ok(output) = runner.output(&CommandLine::new("rustc", &["-vV"])) {
        provenance.target = output
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("host: "))
            .map(|t| t.trim().to_owned());
    }
    provenance
}

/// Cache hit and miss totals from `sccache --show-stats --stats-format=json`.
pub fn sccache_totals(stats: &Value) -> Option<(u64, u64)> {
    let sum = |key: &str| -> Option<u64> {
        let counts = stats.get("stats")?.get(key)?.get("counts")?.as_object()?;
        Some(
            counts
                .values()
                .filter_map(Value::as_u64)
                .fold(0, u64::saturating_add),
        )
    };
    Some((sum("cache_hits")?, sum("cache_misses")?))
}

pub fn sccache_stats(runner: &mut dyn Runner) -> Option<(u64, u64)> {
    let output = runner
        .output(&CommandLine::new(
            "sccache",
            &["--show-stats", "--stats-format=json"],
        ))
        .ok()?;
    sccache_totals(&serde_json::from_str(&output.stdout).ok()?)
}

/// Staging for one build: the layer tree and its archive, removed on drop.
pub struct Staging {
    dir: tempfile::TempDir,
}

impl Staging {
    /// # Errors
    /// Temp dir creation failures.
    pub fn new(worktree: &Worktree) -> Result<Self, CliError> {
        let parent = worktree.root.join("target").join("dev-layers");
        std::fs::create_dir_all(&parent)
            .map_err(CliError::io(format!("create {}", parent.display())))?;
        let dir = tempfile::Builder::new()
            .prefix("layer-")
            .tempdir_in(&parent)
            .map_err(CliError::io("create layer staging"))?;
        Ok(Self { dir })
    }

    pub fn tree(&self) -> PathBuf {
        self.dir.path().join("tree")
    }

    pub fn archive(&self) -> PathBuf {
        self.dir.path().join("layer.tar")
    }
}

/// Summaries for the receipt.
pub fn cache_summary(
    before: Option<(u64, u64)>,
    after: Option<(u64, u64)>,
) -> BTreeMap<&'static str, u64> {
    let mut summary = BTreeMap::new();
    if let (Some((h0, m0)), Some((h1, m1))) = (before, after) {
        summary.insert("sccache_hits", h1.saturating_sub(h0));
        summary.insert("sccache_misses", m1.saturating_sub(m0));
    }
    summary
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tilo_dev_protocol::manifest::Manifest;

    use super::*;

    fn component(text: &str) -> Component {
        Manifest::parse(text)
            .unwrap()
            .components
            .into_values()
            .next()
            .unwrap()
    }

    #[test]
    fn build_uses_manifest_features_plus_dev_overrides() {
        let shell = component(
            "[[component]]\nname = \"browser\"\npackages = [\"browser\", \"helper\"]\nfeatures = [\"browser/wpe\"]\n",
        );
        let metadata = json!({"packages": [
            {"name": "browser", "features": {"wpe": [], "dev-overrides": []}},
            {"name": "helper", "features": {}},
        ]});
        let extra = dev_features(&metadata, &shell.packages);
        assert_eq!(extra, ["browser/dev-overrides"]);
        assert_eq!(
            cargo_build(&shell, &extra).display(),
            "cargo build --locked --profile release -p browser -p helper --features browser/wpe,browser/dev-overrides"
        );
        let plain =
            component("[[component]]\nname = \"x\"\npackages = [\"x\"]\nprofile = \"thin-app\"\n");
        assert_eq!(
            cargo_build(&plain, &[]).display(),
            "cargo build --locked --profile thin-app -p x"
        );
    }

    #[test]
    fn installer_gets_the_profile_dir_and_the_component() {
        let fs = component("[[component]]\nname = \"fs\"\npackages = [\"fs\"]\n");
        assert_eq!(
            install(&fs, Path::new("/stage/tree")).display(),
            "python3 os/scripts/install-components.py --quiet --from target/release --dest /stage/tree fs"
        );
    }

    #[test]
    fn pack_keeps_usr_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        std::fs::write(dir.path().join("usr/bin/shell"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("shell", dir.path().join("usr/bin/sh-link")).unwrap();
        let out = dir.path().join("layer.tar");
        pack(dir.path(), &out).unwrap();
        let mut archive = tar::Archive::new(std::fs::File::open(&out).unwrap());
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().display().to_string())
            .collect();
        assert!(names.contains(&"usr/bin/shell".to_string()), "{names:?}");
        #[cfg(unix)]
        assert!(names.contains(&"usr/bin/sh-link".to_string()));
    }

    #[test]
    fn sccache_json_totals() {
        let stats = json!({"stats": {"cache_hits": {"counts": {"Rust": 40, "C/C++": 2}}, "cache_misses": {"counts": {"Rust": 3}}}});
        assert_eq!(sccache_totals(&stats), Some((42, 3)));
        assert_eq!(
            cache_summary(Some((40, 1)), Some((42, 3)))["sccache_hits"],
            2
        );
        assert!(sccache_totals(&json!({})).is_none());
    }
}
