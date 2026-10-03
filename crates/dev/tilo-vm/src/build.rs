//! `swap --from worktree --builder host`: build a component with plain
//! cargo on this Linux host, lay it out with the manifest installer, and
//! pack the layer for upload. Path decisions stay in
//! `os/scripts/install-components.py`; the cargo and installer arguments
//! are the same ones the in-VM builder runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tilo_vm_protocol::Provenance;
use tilo_vm_protocol::manifest::{Component, dev_override_features};

use crate::context::Worktree;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner};

/// `pkg/dev-overrides` for each of `packages` that declares the feature,
/// read from `cargo metadata`. Production images never enable it.
pub fn dev_features(metadata: &Value, packages: &[String]) -> Vec<String> {
    dev_override_features(metadata, packages)
}

/// Cargo's target directory for this worktree, from `cargo metadata`.
pub fn target_dir(metadata: &Value) -> String {
    metadata
        .get("target_directory")
        .and_then(Value::as_str)
        .map_or_else(|| "target".to_owned(), str::to_owned)
}

/// The cargo host builds run: `TILO_VM_CARGO` if set (any cargo drop-in,
/// such as a wrapper that builds on another machine and hands back the
/// outputs), else `cargo`.
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

pub fn install(component: &Component, target_dir: &str, dest: &Path) -> CommandLine {
    let from = format!("{target_dir}/{}", component.profile_dir());
    let mut command = CommandLine::new("python3", &[]);
    command.args = component.installer_args(&from, &dest.display().to_string());
    command
}

/// `cargo` that builds the app packaging tools, then the `tilo-app` call
/// that packages `apps/<dir>` as a self-signed `.tapp` at `out`.
pub fn app_package(target_dir: &str, dir: &str, out: &Path) -> [CommandLine; 2] {
    let tools = CommandLine::new(
        &cargo_program(),
        &[
            "build",
            "--locked",
            "--release",
            "-p",
            "tilo-app-cli",
            "-p",
            "tilo-app-package-cli",
        ],
    );
    let package = CommandLine::new(&format!("{target_dir}/release/tilo-app"), &["package"])
        .arg(format!("apps/{dir}"))
        .arg("--output")
        .arg(out.display().to_string())
        .arg("--force")
        .env("TILO_PKG", format!("{target_dir}/release/tilo-app-package"));
    [tools, package]
}

/// Apps this worktree offers: `apps/<dir>/tilo-app.toml`.
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

/// What the worktree says about a build of `component`: commit, dirty-tree
/// hash and `Cargo.lock` hash. The builder adds its toolchain.
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
    provenance
}

/// Everything that identifies a host build of `component`, laid out in
/// `tree`. Its target and toolchain are read from the binaries themselves,
/// so a cargo drop-in that builds elsewhere (another architecture, another
/// machine) is recorded as what it built; this host's `rustc` answers only
/// when the layer has no binary that says.
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

/// What a built binary says about its build.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ElfFacts {
    /// `<arch>-unknown-linux-gnu`, from the ELF machine.
    pub target: Option<String>,
    /// `rustc <version> (<hash> <date>)`, as `rustc --version` prints it,
    /// from the `.comment` section rustc writes.
    pub toolchain: Option<String>,
}

/// The facts of the first ELF file under `tree`, in path order.
pub fn first_elf(tree: &Path) -> Option<ElfFacts> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(tree)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries.into_iter().find_map(|path| {
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if meta.is_dir() {
            first_elf(&path)
        } else if meta.is_file() {
            elf_facts(&std::fs::read(&path).ok()?)
        } else {
            None
        }
    })
}

/// Machine and rustc of a little-endian ELF64 file; `None` for anything else.
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
    use tilo_vm_protocol::manifest::Manifest;

    use super::*;

    /// The command as plain cargo would run it, whichever cargo this host picks.
    fn as_cargo(command: &CommandLine) -> String {
        format!("cargo {}", command.args.join(" "))
    }

    /// Host builds run an explicit cargo, else cargo; builds use manifest features plus dev overrides;
    /// the installer gets the profile dir and component; apps package with their own tools.
    #[test]
    fn builds_pick_the_right_cargo_features_and_tools() {
        // Phase: host_builds_run_an_explicit_cargo_else_cargo.
        {
            assert_eq!(cargo_program_from(Some("my-cargo".into())), "my-cargo");
            assert_eq!(cargo_program_from(Some(" ".into())), "cargo");
            assert_eq!(cargo_program_from(None), "cargo");
        }

        // Phase: build_uses_manifest_features_plus_dev_overrides.
        {
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
                as_cargo(&cargo_build(&shell, &extra)),
                "cargo build --locked --profile release -p browser -p helper --features browser/wpe,browser/dev-overrides"
            );
            let plain = component(
                "[[component]]\nname = \"x\"\npackages = [\"x\"]\nprofile = \"thin-app\"\n",
            );
            assert_eq!(
                as_cargo(&cargo_build(&plain, &[])),
                "cargo build --locked --profile thin-app -p x"
            );
        }

        // Phase: installer_gets_the_profile_dir_and_the_component.
        {
            let fs = component("[[component]]\nname = \"fs\"\npackages = [\"fs\"]\n");
            assert_eq!(
                install(&fs, "target", Path::new("/stage/tree")).display(),
                "python3 os/scripts/install-components.py --quiet --from target/release --dest /stage/tree fs"
            );
            assert_eq!(target_dir(&json!({"target_directory": "/t"})), "/t");
            assert_eq!(target_dir(&json!(null)), "target");
        }

        // Phase: apps_package_with_their_own_tools.
        {
            let [tools, package] = app_package("target", "notes", Path::new("/w/notes.tapp"));
            assert_eq!(
                as_cargo(&tools),
                "cargo build --locked --release -p tilo-app-cli -p tilo-app-package-cli"
            );
            assert_eq!(
                package.display(),
                "TILO_PKG=target/release/tilo-app-package target/release/tilo-app package apps/notes --output /w/notes.tapp --force"
            );
            let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
            assert!(apps_in(&repo).contains(&"notes".to_owned()));
        }
    }

    fn component(text: &str) -> Component {
        Manifest::parse(text)
            .unwrap()
            .components
            .into_values()
            .next()
            .unwrap()
    }

    /// A minimal ELF64: header, a `.comment` and the section-name table.
    fn elf(machine: u16, comment: &[u8]) -> Vec<u8> {
        let names = b"\0.comment\0.shstrtab\0";
        let mut bytes = vec![0_u8; 64];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[0x12..0x14].copy_from_slice(&machine.to_le_bytes());
        let len = |b: &[u8]| u64::try_from(b.len()).unwrap();
        let comment_at = len(&bytes);
        bytes.extend_from_slice(comment);
        let names_at = len(&bytes);
        bytes.extend_from_slice(names);
        let headers_at = len(&bytes);
        bytes[0x28..0x30].copy_from_slice(&headers_at.to_le_bytes());
        bytes[0x3a..0x3c].copy_from_slice(&64_u16.to_le_bytes());
        bytes[0x3c..0x3e].copy_from_slice(&3_u16.to_le_bytes());
        bytes[0x3e..0x40].copy_from_slice(&2_u16.to_le_bytes());
        for (name, offset, size) in [
            (0_u32, 0_u64, 0_u64),
            (1, comment_at, len(comment)),
            (10, names_at, len(names)),
        ] {
            let mut header = vec![0_u8; 64];
            header[..4].copy_from_slice(&name.to_le_bytes());
            header[0x18..0x20].copy_from_slice(&offset.to_le_bytes());
            header[0x20..0x28].copy_from_slice(&size.to_le_bytes());
            bytes.extend_from_slice(&header);
        }
        bytes
    }

    /// A host build's target and toolchain come from the binary it made,
    /// so a build for another architecture is recorded as that one.
    #[test]
    fn provenance_reads_target_and_rustc_from_the_built_binary() {
        let comment =
            b"Linker: LLD 22\0rustc version 1.98.0 (88d9e12ae 2026-08-18)\0GCC: (GNU) 16\0";
        assert_eq!(
            elf_facts(&elf(0xb7, comment)),
            Some(ElfFacts {
                target: Some("aarch64-unknown-linux-gnu".into()),
                toolchain: Some("rustc 1.98.0 (88d9e12ae 2026-08-18)".into()),
            })
        );
        assert_eq!(
            elf_facts(&elf(0x3e, b"GCC: (GNU) 16\0")),
            Some(ElfFacts {
                target: Some("x86_64-unknown-linux-gnu".into()),
                toolchain: None,
            })
        );
        assert_eq!(elf_facts(b"#!/bin/sh\n"), None);
        assert_eq!(
            elf_facts(&elf(0xb7, comment)[..40]).map(|f| f.toolchain),
            Some(None)
        );

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        std::fs::create_dir_all(dir.path().join("usr/share/x")).unwrap();
        std::fs::write(dir.path().join("usr/share/x/readme"), b"text").unwrap();
        std::fs::write(dir.path().join("usr/bin/shell"), elf(0xb7, comment)).unwrap();
        assert_eq!(
            first_elf(dir.path()).and_then(|f| f.target).as_deref(),
            Some("aarch64-unknown-linux-gnu")
        );
        // The real binary of this test says what this host builds.
        #[cfg(target_os = "linux")]
        {
            let me = std::fs::read(std::env::current_exe().unwrap()).unwrap();
            assert_eq!(
                elf_facts(&me).and_then(|f| f.target),
                Some(format!("{}-unknown-linux-gnu", std::env::consts::ARCH))
            );
        }
    }

    /// Packing keeps /usr and symlinks, and sccache JSON totals parse.
    #[test]
    fn packing_keeps_usr_links_and_sccache_totals_parse() {
        // Phase: pack_keeps_usr_and_symlinks.
        {
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

        // Phase: sccache_json_totals.
        {
            let stats = json!({"stats": {"cache_hits": {"counts": {"Rust": 40, "C/C++": 2}}, "cache_misses": {"counts": {"Rust": 3}}}});
            assert_eq!(sccache_totals(&stats), Some((42, 3)));
            assert_eq!(
                cache_summary(Some((40, 1)), Some((42, 3)))["sccache_hits"],
                2
            );
            assert!(sccache_totals(&json!({})).is_none());
        }
    }
}
