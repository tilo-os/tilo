//! `swap --from worktree --builder host`: build a component with plain
//! cargo on this Linux host, lay it out with the manifest installer, and
//! pack the layer for upload. Path decisions stay in
//! `os/scripts/install-components.py`; the cargo and installer arguments
//! are the same ones the in-VM builder runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tilo_device_protocol::Provenance;
use tilo_device_protocol::manifest::{Component, dev_override_features};

use crate::context::Worktree;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner};

pub fn cargo_metadata(runner: &mut impl Runner) -> Result<Value, CliError> {
    let command = CommandLine::new(
        &cargo_program(),
        &["metadata", "--locked", "--no-deps", "--format-version", "1"],
    );
    let output = runner.output(&command)?;
    if !output.success {
        return Err(CliError::CommandFailed {
            command: command.display(),
            status: output.status,
            stderr: output.stderr.trim().to_owned(),
        });
    }
    let metadata: Value =
        serde_json::from_str(&output.stdout).map_err(|error| CliError::CommandFailed {
            command: command.display(),
            status: "invalid metadata JSON".into(),
            stderr: error.to_string(),
        })?;
    if !metadata.get("packages").is_some_and(Value::is_array)
        || metadata
            .get("target_directory")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(CliError::CommandFailed {
            command: command.display(),
            status: "incomplete metadata response".into(),
            stderr: "cargo metadata must provide packages and target_directory".into(),
        });
    }
    Ok(metadata)
}

pub fn dev_features(metadata: &Value, packages: &[String]) -> Vec<String> {
    dev_override_features(metadata, packages)
}

pub fn target_dir(metadata: &Value) -> String {
    metadata
        .get("target_directory")
        .and_then(Value::as_str)
        .map_or_else(|| "target".to_owned(), str::to_owned)
}

pub fn cargo_program() -> String {
    cargo_program_from(std::env::var("TILO_VM_CARGO").ok())
}

fn cargo_program_from(explicit: Option<String>) -> String {
    explicit
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "cargo".into())
}

pub fn cargo_build(component: &Component, extra_features: &[String]) -> CommandLine {
    let mut command = CommandLine::new(&cargo_program(), &[]);
    command.args = component.cargo_args(extra_features);
    command
}

const NATIVE_OUTPUTS: &[(&str, &str, &str)] = &[(
    "/usr/lib/tilo/ntfs-mount/fuse-handoff.so",
    "ntfs-mount/fuse-handoff.so",
    "crates/os/ntfs-mount/native/build.py",
)];

pub fn native_builds(component: &Component, target_dir: &str) -> Vec<CommandLine> {
    NATIVE_OUTPUTS
        .iter()
        .filter(|(installed, ..)| component.paths.iter().any(|path| path == installed))
        .map(|(_, output, script)| {
            CommandLine::new("python3", &[script])
                .arg("--out")
                .arg(format!("{target_dir}/{}/{output}", component.profile_dir()))
        })
        .collect()
}

pub fn install(component: &Component, target_dir: &str, dest: &Path) -> CommandLine {
    let from = format!("{target_dir}/{}", component.profile_dir());
    let mut command = CommandLine::new("python3", &[]);
    command.args = component.installer_args(&from, &dest.display().to_string());
    command
}

pub fn app_package(target_dir: &str, dir: &str, out: &Path) -> CommandLine {
    CommandLine::new("python3", &["os/dev/assemble-app-runtime.py"])
        .arg("--repo")
        .arg(".")
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--profile")
        .arg("thin-app")
        .arg("--app")
        .arg(dir)
        .arg("--tapp-out")
        .arg(out.display().to_string())
        .env("CARGO", cargo_program())
}

pub fn apps_in(root: &Path) -> Vec<String> {
    let mut apps: Vec<String> = std::fs::read_dir(root.join("apps"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("tilo-app.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    apps.sort();
    apps
}

pub fn pack(dir: &Path, out: &Path) -> Result<(), CliError> {
    let file =
        std::fs::File::create(out).map_err(CliError::io(format!("create {}", out.display())))?;
    pack_into(dir, file)
}

pub fn pack_into(dir: &Path, out: impl std::io::Write) -> Result<(), CliError> {
    let mut builder = tar::Builder::new(out);
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

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
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

pub fn source_provenance(worktree: &Worktree, component: &str, builder: &str) -> Provenance {
    let mut provenance = Provenance::local(component);
    provenance.source_commit = worktree.head();
    provenance.dirty_tree_hash = worktree
        .dirty_state()
        .map(|state| sha256_hex(state.as_bytes()));
    provenance.builder = Some(builder.into());
    provenance.cargo_lock_hash = std::fs::read(worktree.root.join("Cargo.lock"))
        .ok()
        .map(|b| sha256_hex(&b));
    provenance.interfaces = tilo_device_protocol::tree_interfaces(&worktree.root);
    provenance
}

pub fn provenance(
    runner: &mut dyn Runner,
    worktree: &Worktree,
    component: &Component,
    features: Vec<String>,
    tree: &Path,
) -> Provenance {
    let mut provenance = source_provenance(worktree, &component.name, "host");
    provenance.cargo_profile = Some(component.profile.clone());
    provenance.features = features;
    let built = first_elf(tree).unwrap_or_default();
    provenance.toolchain = built
        .toolchain
        .or_else(|| first_line(runner, &CommandLine::new("rustc", &["--version"])));
    provenance.target = built.target.or_else(|| {
        runner
            .output(&CommandLine::new("rustc", &["-vV"]))
            .ok()?
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("host: "))
            .map(|t| t.trim().to_owned())
    });
    provenance
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ElfFacts {
    pub target: Option<String>,
    /// `rustc <version> (<hash> <date>)`, as `rustc --version` prints it,
    /// from the `.comment` section rustc writes.
    pub toolchain: Option<String>,
}

pub fn first_elf(tree: &Path) -> Option<ElfFacts> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(tree)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    let mut first = None;
    for path in entries {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let facts = if meta.is_dir() {
            first_elf(&path)
        } else if meta.is_file() {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| elf_facts(&bytes))
        } else {
            None
        };
        if let Some(facts) = facts {
            if facts.toolchain.is_some() {
                return Some(facts);
            }
            first.get_or_insert(facts);
        }
    }
    first
}

pub fn elf_facts(bytes: &[u8]) -> Option<ElfFacts> {
    let field = |at: usize, len: usize| bytes.get(at..at.checked_add(len)?);
    let u16_at = |at: usize| Some(u16::from_le_bytes(field(at, 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(field(at, 4)?.try_into().ok()?));
    let u64_at =
        |at: usize| usize::try_from(u64::from_le_bytes(field(at, 8)?.try_into().ok()?)).ok();
    if bytes.get(..6)? != b"\x7fELF\x02\x01" {
        return None;
    }
    let arch = match u16_at(0x12)? {
        0x3e => Some("x86_64"),
        0xb7 => Some("aarch64"),
        0xf3 => Some("riscv64gc"),
        _ => None,
    };
    let section = |index: usize| -> Option<(usize, usize, usize)> {
        let header = u64_at(0x28)?.checked_add(index.checked_mul(usize::from(u16_at(0x3a)?))?)?;
        Some((
            usize::try_from(u32_at(header)?).ok()?,
            u64_at(header.checked_add(0x18)?)?,
            u64_at(header.checked_add(0x20)?)?,
        ))
    };
    let toolchain = (|| {
        let (_, names, names_size) = section(usize::from(u16_at(0x3e)?))?;
        let names = bytes.get(names..names.checked_add(names_size)?)?;
        (0..usize::from(u16_at(0x3c)?)).find_map(|index| {
            let (name, offset, size) = section(index)?;
            let name = names.get(name..)?.split(|b| *b == 0).next()?;
            if name != b".comment" {
                return None;
            }
            bytes
                .get(offset..offset.checked_add(size)?)?
                .split(|b| *b == 0)
                .find_map(|line| {
                    let line = std::str::from_utf8(line).ok()?;
                    line.strip_prefix("rustc version ")
                        .map(|version| format!("rustc {version}"))
                })
        })
    })();
    Some(ElfFacts {
        target: arch.map(|arch| format!("{arch}-unknown-linux-gnu")),
        toolchain,
    })
}

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

pub struct Staging {
    dir: tempfile::TempDir,
}

impl Staging {
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
