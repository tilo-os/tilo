//! The maintained producer builds a fixture inventory, not a copied build loop.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

use super::*;
use crate::{context::Worktree, source_inputs::Snapshot};
use tilo_vm_protocol::USERSPACE_COMPONENTS;

type FixtureResult<T> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn offline_preparation_checks_the_real_producer_and_refuses_incomplete_or_foreign_cohorts() {
    let fixture = Fixture::with_native_payload().unwrap();
    let output = fixture.run("batch", None).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest =
        tilo_vm_protocol::Manifest::load(&fixture.root.path().join("os/components.toml"))
            .unwrap()
            .unwrap();
    let arch = std::env::consts::ARCH;
    let verify = |bytes: &[u8], digest: &str, arch: &str| {
        crate::app::userspace_prepare::verify_prepared_stream(
            &mut Stream::new(bytes),
            &manifest,
            digest,
            arch,
        )
    };
    let prepared = verify(&output.stdout, &fixture.snapshot.digest, arch).unwrap();
    assert_eq!(prepared.len(), USERSPACE_COMPONENTS.len());
    assert_eq!(
        prepared
            .iter()
            .map(|member| member.component.as_str())
            .collect::<Vec<_>>(),
        USERSPACE_COMPONENTS
    );
    assert!(verify(&output.stdout, "foreign-source", arch).is_err());
    assert!(
        verify(
            &output.stdout,
            &fixture.snapshot.digest,
            "foreign-architecture"
        )
        .is_err()
    );
    let last_end = output
        .stdout
        .windows(END.len())
        .rposition(|bytes| bytes == END.as_bytes())
        .expect("fixture producer includes the final payload checksum");
    let cut = &output.stdout[..last_end + END.len() + 12];
    assert!(verify(cut, &fixture.snapshot.digest, arch).is_err());
    let mut corrupt = output.stdout.clone();
    let data = corrupt
        .windows(DATA.len())
        .position(|bytes| bytes == DATA.as_bytes())
        .expect("fixture producer includes payload bytes")
        + DATA.len();
    corrupt[data] = if corrupt[data] == b'A' { b'B' } else { b'A' };
    assert!(verify(&corrupt, &fixture.snapshot.digest, arch).is_err());
    let mut extra = output.stdout.clone();
    extra.extend_from_slice(format!("{BEGIN}{{}}\n").as_bytes());
    assert!(verify(&extra, &fixture.snapshot.digest, arch).is_err());
}

fn run_producer_child() -> FixtureResult<bool> {
    let Some(mode) = std::env::var_os("TILO_COHORT_FIXTURE_MODE") else {
        return Ok(false);
    };
    let inventory = std::path::PathBuf::from(
        std::env::var_os("TILO_COHORT_FIXTURE_INVENTORY").ok_or("missing fixture inventory")?,
    );
    if mode == "batch" {
        pack_userspace(&inventory)?;
    } else {
        let manifest = tilo_vm_protocol::Manifest::load(Path::new("os/components.toml"))?
            .ok_or("missing fixture manifest")?;
        for name in USERSPACE_COMPONENTS {
            let component = manifest.get(name).ok_or("missing fixture component")?;
            pack_with_source(name, &component.profile, &[], Some(&inventory), &[])?;
        }
    }
    Ok(true)
}

struct Fixture {
    root: tempfile::TempDir,
    snapshot: Snapshot,
}

impl Fixture {
    fn with_native_payload() -> FixtureResult<Self> {
        let mut fixture = Self::new()?;
        // The original byte-stream fixture intentionally has non-ELF members.
        // This admission fixture uses a real small native executable so the
        // maintained producer independently observes its architecture.
        std::fs::copy(
            "/usr/bin/true",
            fixture.root.path().join("target/fixture-architecture-elf"),
        )?;
        for script in [
            "os/scripts/install-components.py",
            "os/dev/assemble-app-runtime.py",
        ] {
            let path = fixture.root.path().join(script);
            let text = std::fs::read_to_string(&path)?;
            if !text.contains("(p/'binary').write_bytes(material)") {
                return Err("fixture installer no longer exposes its native payload".into());
            }
            std::fs::write(
                path,
                text.replace(
                    "(p/'binary').write_bytes(material)",
                    "shutil.copyfile('target/fixture-architecture-elf',p/'binary')",
                ),
            )?;
        }
        let worktree = Worktree::detect(fixture.root.path()).ok_or("missing fixture worktree")?;
        fixture.snapshot = Snapshot::create(&worktree, "userspace")?;
        Ok(fixture)
    }

    fn new() -> FixtureResult<Self> {
        let root = tempfile::tempdir()?;
        for dir in ["os/scripts", "os/dev", "target/release"] {
            std::fs::create_dir_all(root.path().join(dir))?;
        }
        // Exercise production ELF provenance using this real native executable.
        // The fixture package is synthetic and never claims to be a usable SDK.
        std::fs::copy(
            std::env::current_exe()?,
            root.path().join("target/release/libruntime_dylib.so"),
        )?;
        let mut manifest = String::new();
        for name in USERSPACE_COMPONENTS {
            writeln!(
                &mut manifest,
                "[[component]]\nname = {name:?}\nkind = \"userspace\"\nscope = \"session\"\npackages = [{name:?}]\nprofile = \"release\"\n{}\n",
                if *name == "apps+runtime" {
                    "layer = \"direct\""
                } else {
                    ""
                }
            )?;
        }
        std::fs::write(root.path().join("os/components.toml"), manifest)?;
        let installer = r"import hashlib,pathlib,shutil,sys
args=sys.argv
material=pathlib.Path('target/fixture-built-material').read_bytes()
if '--input-key' in args:
 digest=args[args.index('--source-digest')+1]
 sdk=pathlib.Path('target/release/libruntime_dylib.so').read_bytes()
 print(hashlib.sha256(material+hashlib.sha256(sdk).digest()+digest.encode()).hexdigest())
 sys.exit(0)
p=pathlib.Path(args[args.index('--dest')+1]); p.mkdir(parents=True)
(p/'binary').write_bytes(material)
if sys.argv[0].endswith('assemble-app-runtime.py'):
 lib=p/'usr/lib/tilo-runtime/fixture/lib/libruntime_dylib.so'
 lib.parent.mkdir(parents=True)
 shutil.copyfile('target/release/libruntime_dylib.so',lib)
";
        for path in [
            "os/scripts/install-components.py",
            "os/dev/assemble-app-runtime.py",
        ] {
            std::fs::write(root.path().join(path), installer)?;
        }
        let cargo = r"#!/usr/bin/env python3
import json,pathlib,sys,time,os
root=pathlib.Path.cwd()
with (root/'target/events').open('a') as f: f.write(json.dumps(sys.argv[1:])+'\n')
if sys.argv[1]=='metadata':
 time.sleep(0.02)
 print(json.dumps({'target_directory':str(root/'target'),'packages':[]}))
elif os.environ.get('TILO_COHORT_MUTATE') == '1':
 with (root/'fixture-cargo').open('a') as f: f.write('\n# source changed during build\n')
elif os.environ.get('TILO_COHORT_FAIL') in sys.argv[1:]:
 sys.exit(9)
else:
 (root/'target/fixture-built-material').write_bytes(b'fixture built material')
";
        let path = root.path().join("fixture-cargo");
        std::fs::write(&path, cargo)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        for args in [
            vec!["init", "-q"],
            vec!["add", "os", "fixture-cargo"],
            vec![
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-qm",
                "fixture",
            ],
        ] {
            if !Command::new("git")
                .current_dir(root.path())
                .args(args)
                .status()?
                .success()
            {
                return Err("fixture git preparation failed".into());
            }
        }
        let worktree = Worktree::detect(root.path()).ok_or("missing fixture worktree")?;
        let snapshot = Snapshot::create(&worktree, "userspace")?;
        Ok(Self { root, snapshot })
    }

    fn run(&self, mode: &str, fail: Option<&str>) -> std::io::Result<std::process::Output> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .current_dir(self.root.path())
            .args([
                "builderlayer::batch_tests::userspace_batch_keeps_real_archives_and_removes_repeated_metadata",
                "--exact",
                "--nocapture",
            ])
            .env("CARGO", self.root.path().join("fixture-cargo"))
            .env("TILO_COHORT_FIXTURE_MODE", mode)
            .env("TILO_COHORT_FIXTURE_INVENTORY", self.snapshot.file.path());
        if let Some(fail) = fail {
            command.env("TILO_COHORT_FAIL", fail);
        } else {
            command.env_remove("TILO_COHORT_FAIL");
        }
        command.output()
    }

    fn events(&self) -> FixtureResult<Vec<Vec<String>>> {
        Ok(
            std::fs::read_to_string(self.root.path().join("target/events"))?
                .lines()
                .map(serde_json::from_str)
                .collect::<Result<_, _>>()?,
        )
    }
}

#[test]
fn userspace_batch_keeps_real_archives_and_removes_repeated_metadata() {
    if run_producer_child().unwrap() {
        return;
    }
    let fixture = Fixture::new().unwrap();
    let start = std::time::Instant::now();
    let old = fixture.run("single", None).unwrap();
    let old_elapsed = start.elapsed();
    assert!(
        old.status.success(),
        "{}",
        String::from_utf8_lossy(&old.stderr)
    );
    assert_eq!(
        fixture
            .events()
            .unwrap()
            .iter()
            .filter(|args| args[0] == "metadata")
            .count(),
        9
    );
    std::fs::remove_file(fixture.root.path().join("target/events")).unwrap();
    let start = std::time::Instant::now();
    let new = fixture.run("batch", None).unwrap();
    let new_elapsed = start.elapsed();
    assert!(
        new.status.success(),
        "{}",
        String::from_utf8_lossy(&new.stderr)
    );
    let events = fixture.events().unwrap();
    assert_eq!(
        events.iter().filter(|args| args[0] == "metadata").count(),
        1
    );
    assert_eq!(events.iter().filter(|args| args[0] == "build").count(), 8);
    for args in events.iter().filter(|args| args[0] == "build") {
        assert!(args.iter().any(|arg| arg == "--locked"));
        assert!(args.windows(2).any(|pair| pair == ["--profile", "release"]));
    }
    let decode = |bytes: &[u8]| {
        let mut stream = Stream::new(bytes);
        let mut layers = Vec::new();
        for name in USERSPACE_COMPONENTS {
            if !layers.is_empty() {
                stream.next_layer().unwrap();
            }
            let packed = stream.header(&mut Vec::new()).unwrap();
            assert_eq!(&packed.component, name);
            assert_eq!(
                packed.source_digest.as_deref(),
                Some(fixture.snapshot.digest.as_str())
            );
            let mut archive = Vec::new();
            stream.read_to_end(&mut archive).unwrap();
            assert!(!archive.is_empty());
            layers.push(archive);
        }
        stream.finish(&mut Vec::new()).unwrap();
        layers
    };
    assert_eq!(decode(&old.stdout), decode(&new.stdout));
    fixture.snapshot.unchanged(fixture.root.path()).unwrap();
    eprintln!(
        "real producer fixture: single-layer helper calls9 metadata9 elapsed={old_elapsed:?}; batched metadata1 elapsed={new_elapsed:?}; production remote command count9→1, hardware latency unmeasured"
    );
}

#[test]
fn userspace_batch_failure_stops_without_emitting_later_members() {
    let fixture = Fixture::new().unwrap();
    let output = fixture.run("batch", Some("control")).unwrap();
    assert!(!output.status.success());
    let mut stream = Stream::new(output.stdout.as_slice());
    for (index, name) in USERSPACE_COMPONENTS.iter().take(5).enumerate() {
        if index > 0 {
            stream.next_layer().unwrap();
        }
        assert_eq!(stream.header(&mut Vec::new()).unwrap().component, *name);
        stream.read_to_end(&mut Vec::new()).unwrap();
    }
    stream.next_layer().unwrap();
    assert!(stream.header(&mut Vec::new()).is_err());
    assert!(
        !fixture
            .events()
            .unwrap()
            .iter()
            .any(|args| args.iter().any(|arg| arg == "shell"))
    );
}

#[test]
fn userspace_batch_cannot_advance_unverified_or_accept_extra_archives() {
    let mut frame = Vec::new();
    writeln!(
        &mut frame,
        "{BEGIN}{{\"component\":\"shell\",\"target\":null,\"toolchain\":null}}"
    )
    .unwrap();
    let mut lines = Lines::new(&mut frame);
    lines.write_all(b"actual layer").unwrap();
    let (hash, bytes) = lines.finish().unwrap();
    writeln!(&mut frame, "{END}{hash} {bytes}").unwrap();
    let mut doubled = frame.clone();
    doubled.extend(&frame);
    let mut stream = Stream::new(doubled.as_slice());
    stream.header(&mut Vec::new()).unwrap();
    assert!(stream.next_layer().is_err());
    stream.read_to_end(&mut Vec::new()).unwrap();
    assert!(stream.finish(&mut Vec::new()).is_err());
    // A refused stream is terminal; a fresh stream demonstrates valid advancement.
    let mut stream = Stream::new(doubled.as_slice());
    stream.header(&mut Vec::new()).unwrap();
    stream.read_to_end(&mut Vec::new()).unwrap();
    stream.next_layer().unwrap();
    stream.header(&mut Vec::new()).unwrap();
    stream.read_to_end(&mut Vec::new()).unwrap();
    stream.finish(&mut Vec::new()).unwrap();
}

#[test]
fn userspace_batch_refuses_changed_source_before_emitting_a_layer() {
    let fixture = Fixture::new().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .current_dir(fixture.root.path())
        .args(["builderlayer::batch_tests::userspace_batch_keeps_real_archives_and_removes_repeated_metadata", "--exact", "--nocapture"])
        .env("CARGO", fixture.root.path().join("fixture-cargo"))
        .env("TILO_COHORT_FIXTURE_MODE", "batch")
        .env("TILO_COHORT_FIXTURE_INVENTORY", fixture.snapshot.file.path())
        .env("TILO_COHORT_MUTATE", "1")
        .output().unwrap();
    assert!(!output.status.success());
    let mut stream = Stream::new(output.stdout.as_slice());
    assert!(stream.header(&mut Vec::new()).is_err());
    assert!(fixture.snapshot.unchanged(fixture.root.path()).is_err());
    assert_eq!(
        fixture
            .events()
            .unwrap()
            .iter()
            .filter(|args| args[0] == "build")
            .count(),
        1
    );
}

#[test]
fn userspace_batch_command_keeps_configured_builder_and_inventory() {
    let command = userspace_command(
        &[
            "cargo-drop-in".into(),
            "--config".into(),
            "owned-builder.toml".into(),
        ],
        Path::new(".tilo-vm-inputs-owned.json"),
    );
    assert_eq!(command.program, "cargo-drop-in");
    assert_eq!(
        command.args,
        [
            "--config",
            "owned-builder.toml",
            "run",
            "--locked",
            "-p",
            "tilo-vm",
            "--bin",
            "tilo-vm",
            "--",
            "userspace-pack",
            "--source-inventory",
            ".tilo-vm-inputs-owned.json"
        ]
    );
}
