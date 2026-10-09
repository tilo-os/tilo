//! Durable local cache of the existing framed producer stream. This is local
//! developer provenance, never a new CI publisher or device admission protocol.

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tilo_vm_protocol::{Manifest, Provenance, USERSPACE_COMPONENTS};

use super::super::userspace_builder::space;
use super::stage_prepared_stream;
use crate::builderlayer::{Packed, Stream};
use crate::error::CliError;
use crate::source_inputs::Snapshot;

const MAX_STREAM: u64 = 64 << 30;
const MAX_DESCRIPTOR: u64 = 32 << 20;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Prepared {
    version: u32,
    pub arch: String,
    source_inventory: String,
    source: Provenance,
    stream_sha256: String,
    members: Vec<Packed>,
    pub metadata: serde_json::Value,
}

pub(super) struct Capture {
    directory: tempfile::TempDir,
    stream: File,
    digest: String,
}

pub(in crate::app) struct Loaded {
    pub prepared: Prepared,
    pub members: Vec<(Packed, space::Archive)>,
    _spool: tempfile::TempDir,
}

fn invalid(message: &str) -> CliError {
    CliError::InvalidInput(format!("prepared userspace: {message}"))
}

struct Recorded<R, W> {
    input: R,
    output: W,
    hash: Sha256,
    bytes: u64,
}

impl<R: Read, W: Write> Read for Recorded<R, W> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let length = self.input.read(bytes)?;
        self.bytes = self
            .bytes
            .checked_add(length as u64)
            .ok_or_else(|| std::io::Error::other("prepared userspace stream size overflow"))?;
        if self.bytes > MAX_STREAM {
            return Err(std::io::Error::other(
                "prepared userspace stream exceeds 64 GiB",
            ));
        }
        let recorded = bytes.get(..length).ok_or_else(|| {
            std::io::Error::other("prepared userspace reader exceeded its buffer")
        })?;
        self.output.write_all(recorded)?;
        self.hash.update(recorded);
        Ok(length)
    }
}

impl Capture {
    pub(super) fn new(root: &Path) -> Result<Self, CliError> {
        let parent = root.join("target/tilo-vm/prepared");
        std::fs::create_dir_all(&parent)
            .map_err(CliError::io("create prepared userspace cache"))?;
        let directory = tempfile::Builder::new()
            .prefix("pending-")
            .tempdir_in(&parent)
            .map_err(CliError::io("create private prepared userspace spool"))?;
        let stream = File::create_new(directory.path().join("cohort.stream"))
            .map_err(CliError::io("create prepared userspace stream"))?;
        Ok(Self {
            directory,
            stream,
            digest: String::new(),
        })
    }

    pub(super) fn receive(
        &mut self,
        input: impl Read,
        manifest: &Manifest,
        inputs: &Snapshot,
        arch: &str,
    ) -> Result<Vec<Packed>, CliError> {
        let mut recorded = Recorded {
            input,
            output: &mut self.stream,
            hash: Sha256::new(),
            bytes: 0,
        };
        let staged = stage_prepared_stream(
            &mut Stream::new(BufReader::with_capacity(2 << 20, &mut recorded)),
            manifest,
            &inputs.digest,
            arch,
            self.directory.path(),
        )?;
        let members = staged.into_iter().map(|(packed, _)| packed).collect();
        self.digest = hex::encode(recorded.hash.finalize());
        self.stream
            .sync_all()
            .map_err(CliError::io("persist prepared userspace stream"))?;
        Ok(members)
    }

    pub(super) fn publish(
        self,
        inputs: &Snapshot,
        arch: &str,
        members: Vec<Packed>,
        metadata: serde_json::Value,
    ) -> Result<PathBuf, CliError> {
        if self.digest.len() != 64 || members.len() != USERSPACE_COMPONENTS.len() {
            return Err(invalid("incomplete preparation cannot be published"));
        }
        let prepared = Prepared {
            version: 1,
            arch: arch.into(),
            source_inventory: inputs.digest.clone(),
            source: inputs.provenance.clone(),
            stream_sha256: self.digest,
            members,
            metadata,
        };
        let bytes = serde_json::to_vec(&prepared).map_err(|error| invalid(&error.to_string()))?;
        if bytes.len() as u64 > MAX_DESCRIPTOR {
            return Err(invalid("descriptor exceeds 32 MiB"));
        }
        let mut descriptor = File::create_new(self.directory.path().join("bundle.json"))
            .map_err(CliError::io("create prepared userspace descriptor"))?;
        descriptor
            .write_all(&bytes)
            .map_err(CliError::io("write prepared userspace descriptor"))?;
        descriptor
            .sync_all()
            .map_err(CliError::io("persist prepared userspace descriptor"))?;
        drop(descriptor);
        drop(self.stream);
        let parent = self
            .directory
            .path()
            .parent()
            .ok_or_else(|| invalid("cache has no parent"))?;
        let path = parent.join(crate::build::sha256_hex(&bytes));
        // A second preparation may produce the exact same descriptor and bytes.
        // Reuse only after checking those retained bytes against this new,
        // fully verified producer output; never replace a published directory.
        if path
            .try_exists()
            .map_err(CliError::io("inspect prepared cache"))?
        {
            let mut retained = Vec::new();
            File::open(path.join("bundle.json"))
                .map_err(CliError::io("open retained prepared descriptor"))?
                .take(MAX_DESCRIPTOR + 1)
                .read_to_end(&mut retained)
                .map_err(CliError::io("read retained prepared descriptor"))?;
            let mut stream = Recorded {
                input: File::open(path.join("cohort.stream"))
                    .map_err(CliError::io("open retained prepared stream"))?,
                output: std::io::sink(),
                hash: Sha256::new(),
                bytes: 0,
            };
            std::io::copy(&mut stream, &mut std::io::sink())
                .map_err(CliError::io("verify retained prepared stream"))?;
            if retained != bytes || hex::encode(stream.hash.finalize()) != prepared.stream_sha256 {
                return Err(invalid(
                    "existing cache does not match verified preparation",
                ));
            }
            return Ok(path);
        }
        std::fs::rename(self.directory.path(), &path)
            .map_err(CliError::io("publish verified prepared userspace"))?;
        Ok(path)
    }
}

pub(in crate::app) fn load(
    path: &Path,
    manifest: &Manifest,
    inputs: &Snapshot,
    root: &Path,
) -> Result<Loaded, CliError> {
    let mut bytes = Vec::new();
    File::open(path.join("bundle.json"))
        .map_err(CliError::io("read prepared descriptor"))?
        .take(MAX_DESCRIPTOR + 1)
        .read_to_end(&mut bytes)
        .map_err(CliError::io("read bounded prepared descriptor"))?;
    if bytes.len() as u64 > MAX_DESCRIPTOR
        || path.file_name().and_then(|name| name.to_str())
            != Some(crate::build::sha256_hex(&bytes).as_str())
    {
        return Err(invalid(
            "descriptor does not match its immutable cache identity",
        ));
    }
    let prepared: Prepared = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(&format!("invalid descriptor: {error}")))?;
    if prepared.version != 1
        || !matches!(prepared.arch.as_str(), "x86_64" | "aarch64")
        || prepared.source_inventory != inputs.digest
        || prepared.source != inputs.provenance
        || prepared.members.len() != USERSPACE_COMPONENTS.len()
    {
        return Err(invalid("descriptor is stale, foreign or incomplete"));
    }
    inputs.unchanged(root)?;
    let parent = root.join("target/tilo-vm");
    std::fs::create_dir_all(&parent)
        .map_err(CliError::io("create prepared archive spool parent"))?;
    let spool = tempfile::Builder::new()
        .prefix("prepared-apply-")
        .tempdir_in(parent)
        .map_err(CliError::io("create private prepared archive spool"))?;
    let mut recorded = Recorded {
        input: File::open(path.join("cohort.stream"))
            .map_err(CliError::io("open prepared cohort stream"))?,
        output: std::io::sink(),
        hash: Sha256::new(),
        bytes: 0,
    };
    let mut stream = Stream::new(BufReader::with_capacity(2 << 20, &mut recorded));
    let members = stage_prepared_stream(
        &mut stream,
        manifest,
        &inputs.digest,
        &prepared.arch,
        spool.path(),
    )?;
    if members
        .iter()
        .map(|(packed, _)| packed)
        .ne(prepared.members.iter())
    {
        return Err(invalid("member header differs from verified preparation"));
    }
    drop(stream);
    if hex::encode(recorded.hash.finalize()) != prepared.stream_sha256 {
        return Err(invalid("stream bytes differ from verified preparation"));
    }
    inputs.unchanged(root)?;
    Ok(Loaded {
        prepared,
        members,
        _spool: spool,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::builderlayer::batch_tests::Fixture;
    use crate::context::Worktree;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn retained_userspace_replays_real_verified_producer_and_refuses_corruption_and_stale_source() {
        let fixture = Fixture::with_native_payload().unwrap();
        let output = fixture.run("batch", None).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest = Manifest::load(&fixture.root.path().join("os/components.toml"))
            .unwrap()
            .unwrap();
        let arch = std::env::consts::ARCH;
        // A cache-reference header is not a retained complete payload, even if
        // the producer's original framed bytes remain after the altered header.
        let begin = output
            .stdout
            .windows(crate::builderlayer::BEGIN.len())
            .position(|bytes| bytes == crate::builderlayer::BEGIN.as_bytes())
            .unwrap();
        let header_start = begin + crate::builderlayer::BEGIN.len();
        let header_end = header_start
            + output.stdout[header_start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap();
        let mut reused: Packed =
            serde_json::from_slice(&output.stdout[header_start..header_end]).unwrap();
        reused.reused = true;
        let mut reference = output.stdout[..header_start].to_vec();
        reference.extend(serde_json::to_vec(&reused).unwrap());
        reference.extend(&output.stdout[header_end..]);
        let mut rejected_capture = Capture::new(fixture.root.path()).unwrap();
        assert!(
            rejected_capture
                .receive(reference.as_slice(), &manifest, &fixture.snapshot, arch)
                .is_err()
        );
        let rejected_path = rejected_capture.directory.path().to_owned();
        drop(rejected_capture);
        assert!(!rejected_path.exists());
        let mut capture = Capture::new(fixture.root.path()).unwrap();
        let members = capture
            .receive(output.stdout.as_slice(), &manifest, &fixture.snapshot, arch)
            .unwrap();
        let path = capture
            .publish(
                &fixture.snapshot,
                arch,
                members.clone(),
                serde_json::json!({"packages": []}),
            )
            .unwrap();
        let mut repeated = Capture::new(fixture.root.path()).unwrap();
        let repeated_members = repeated
            .receive(output.stdout.as_slice(), &manifest, &fixture.snapshot, arch)
            .unwrap();
        assert_eq!(
            repeated
                .publish(
                    &fixture.snapshot,
                    arch,
                    repeated_members,
                    serde_json::json!({"packages": []})
                )
                .unwrap(),
            path
        );
        let loaded = load(&path, &manifest, &fixture.snapshot, fixture.root.path()).unwrap();
        assert_eq!(loaded.members.len(), USERSPACE_COMPONENTS.len());
        assert_eq!(
            loaded
                .members
                .iter()
                .map(|(packed, _)| packed.clone())
                .collect::<Vec<_>>(),
            members
        );
        assert_eq!(loaded.prepared.source, fixture.snapshot.provenance);
        drop(loaded);
        // Complete checksum, framing and tar semantics are checked again on reuse.
        let original = std::fs::read(path.join("cohort.stream")).unwrap();
        let mut corrupt = original.clone();
        let data = corrupt
            .windows(crate::builderlayer::DATA.len())
            .position(|part| part == crate::builderlayer::DATA.as_bytes())
            .unwrap()
            + crate::builderlayer::DATA.len();
        corrupt[data] ^= 1;
        std::fs::write(path.join("cohort.stream"), &corrupt).unwrap();
        assert!(load(&path, &manifest, &fixture.snapshot, fixture.root.path()).is_err());
        std::fs::write(path.join("cohort.stream"), &original[..original.len() / 2]).unwrap();
        assert!(load(&path, &manifest, &fixture.snapshot, fixture.root.path()).is_err());
        std::fs::write(path.join("cohort.stream"), &original).unwrap();
    }

    #[test]
    fn retained_userspace_refuses_readdressed_metadata_and_stale_source() {
        let fixture = Fixture::with_native_payload().unwrap();
        let output = fixture.run("batch", None).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest = Manifest::load(&fixture.root.path().join("os/components.toml"))
            .unwrap()
            .unwrap();
        let arch = std::env::consts::ARCH;
        let mut capture = Capture::new(fixture.root.path()).unwrap();
        let members = capture
            .receive(output.stdout.as_slice(), &manifest, &fixture.snapshot, arch)
            .unwrap();
        let path = capture
            .publish(
                &fixture.snapshot,
                arch,
                members,
                serde_json::json!({"packages": []}),
            )
            .unwrap();
        let original = std::fs::read(path.join("cohort.stream")).unwrap();
        // Even syntactically valid metadata edits cannot keep the cache identity.
        let descriptor = std::fs::read(path.join("bundle.json")).unwrap();
        let mut changed: Prepared = serde_json::from_slice(&descriptor).unwrap();
        changed.arch = "foreign-architecture".into();
        std::fs::write(
            path.join("bundle.json"),
            serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        assert!(load(&path, &manifest, &fixture.snapshot, fixture.root.path()).is_err());
        std::fs::write(path.join("bundle.json"), &descriptor).unwrap();
        // Re-addressing a foreign descriptor still cannot admit a wrong target.
        changed.arch = if arch == "x86_64" {
            "aarch64"
        } else {
            "x86_64"
        }
        .into();
        let changed_bytes = serde_json::to_vec(&changed).unwrap();
        let foreign = path
            .parent()
            .unwrap()
            .join(crate::build::sha256_hex(&changed_bytes));
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("bundle.json"), changed_bytes).unwrap();
        std::fs::write(foreign.join("cohort.stream"), &original).unwrap();
        assert!(load(&foreign, &manifest, &fixture.snapshot, fixture.root.path()).is_err());
        // All canonical nine members are required, even if a descriptor is re-addressed.
        changed.arch = arch.into();
        changed.members.pop();
        let bytes = serde_json::to_vec(&changed).unwrap();
        let incomplete = path
            .parent()
            .unwrap()
            .join(crate::build::sha256_hex(&bytes));
        std::fs::create_dir(&incomplete).unwrap();
        std::fs::write(incomplete.join("bundle.json"), bytes).unwrap();
        std::fs::write(incomplete.join("cohort.stream"), &original).unwrap();
        assert!(
            load(
                &incomplete,
                &manifest,
                &fixture.snapshot,
                fixture.root.path()
            )
            .is_err()
        );
        std::fs::write(fixture.root.path().join("fixture-cargo"), "changed source").unwrap();
        assert!(load(&path, &manifest, &fixture.snapshot, fixture.root.path()).is_err());
        let current =
            Snapshot::create(&Worktree::detect(fixture.root.path()).unwrap(), "userspace").unwrap();
        assert!(load(&path, &manifest, &current, fixture.root.path()).is_err());
    }

    #[test]
    fn failed_preparation_cannot_publish_and_private_spool_is_removed() {
        let fixture = Fixture::with_native_payload().unwrap();
        let capture = Capture::new(fixture.root.path()).unwrap();
        let pending = capture.directory.path().to_owned();
        assert!(
            capture
                .publish(
                    &fixture.snapshot,
                    std::env::consts::ARCH,
                    vec![],
                    serde_json::Value::Null
                )
                .is_err()
        );
        assert!(!pending.exists());
    }
    // This peer observes production client ordering and CAS requests. It does
    // not stand in for VMD signature, package-closure or ownership admission.
    const APPLY_PEER: &str = r"#!/usr/bin/env python3
import base64,json,os,pathlib,sys
root=pathlib.Path(os.environ['TILO_PREPARED_FIXTURE_ROOT'])
prepared=json.loads(pathlib.Path(os.environ['TILO_PREPARED_FIXTURE_PATH'],'bundle.json').read_text())
mode=os.environ['TILO_PREPARED_FIXTURE_MODE']
protocol=int(os.environ['TILO_PREPARED_FIXTURE_PROTOCOL'])
session={'user':'fixture','unit':'fixture.service','login_session':'fixture-login','login_wrapper':21}
received=0
for line in sys.stdin:
 request=json.loads(line); method=request['method']; params=request['params']
 with (root/'target/calls').open('a') as f: f.write(json.dumps(request)+'\n')
 if method=='hello':
  arch=prepared['arch'] if mode!='foreign' else ('aarch64' if prepared['arch']=='x86_64' else 'x86_64')
  result={'protocol':protocol-(1 if mode=='protocol' else 0),'daemon_version':'fixture','root':'fixture','arch':arch,'capabilities':['userspace.apply']}
 elif method=='status':
  slot={'name':'device','kind':'device','owner':{},'user':'fixture','generation':17,'generations':[0,17],'components':{},'session':'active','seat':None,'session_identity':session}
  result={'protocol':protocol,'daemon_version':'fixture','slots':[slot],'seat0':None,'storage':{'layers_bytes':0,'free_bytes':1<<40,'total_bytes':1<<40},'issues':[]}
 elif method=='layer.upload.binary':
  print(json.dumps({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32601,'message':'fixture exercises supported legacy transfer','hint':'use the supported chunk transfer'}}),flush=True); continue
 elif method=='layer.upload.begin':
  received=0; result={'upload':'fixture-upload'}
 elif method=='layer.upload.chunk':
  received+=len(base64.b64decode(params['data'])); result={'received':received}
 elif method=='layer.import':
  counter=root/'target/import-count'; index=int(counter.read_text()) if counter.exists() else 0
  counter.write_text(str(index+1))
  result={'hash':prepared['members'][index]['layer_hash'] if mode!='hash' else 'f'*64,'existed':False}
 elif method=='userspace.apply':
  assert params['expected_generation']==17 and params['expected_session']==session
  assert params['source_inventory']==prepared['source_inventory'] and params['source']==prepared['source']
  assert [m['component'] for m in params['members']]==[m['component'] for m in prepared['members']]
  assert [m['layer'] for m in params['members']]==[m['layer_hash'] for m in prepared['members']]
  assert all(m['source_inventory']==prepared['source_inventory'] for m in params['members'])
  assert int((root/'target/import-count').read_text())==9
  if mode=='cas':
   print(json.dumps({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32602,'message':'fixture generation changed','hint':'refresh the observed slot generation'}}),flush=True); continue
  result={'apply':{'slot':'device','generation':18,'changed':True,'policy':'session_restart','actions':[]},'source':params['source'],'source_inventory':params['source_inventory'],'members':params['members'],'phases':[],'failure':None}
 else: raise RuntimeError(method)
 print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
";

    fn run_prepared_swap_child() -> Result<bool, CliError> {
        let Some(root) = std::env::var_os("TILO_PREPARED_FIXTURE_ROOT") else {
            return Ok(false);
        };
        let root = PathBuf::from(root);
        let path = PathBuf::from(
            std::env::var_os("TILO_PREPARED_FIXTURE_PATH")
                .ok_or_else(|| invalid("fixture path is missing"))?,
        );
        let mode = std::env::var("TILO_PREPARED_FIXTURE_MODE")
            .map_err(|_| invalid("fixture mode is missing"))?;
        let mut app = crate::app::App {
            json: true,
            target: Some("fixture".into()),
            worktree: Worktree::detect(&root),
            registry_path: root.join("target/vms.toml"),
            runner: crate::hypervisor::SystemRunner { cwd: None },
            receipt: crate::receipt::Receipt::new(&root, vec![]),
            warnings: Vec::new(),
            peer: None,
        };
        let result = app.swap_userspace(
            "device",
            &format!("prepared:{}", path.display()),
            None,
            None,
        );
        if mode == "success" {
            result?;
        } else {
            let error = result
                .err()
                .ok_or_else(|| invalid("fixture mode must refuse"))?;
            let reason = match mode.as_str() {
                "protocol" => {
                    let CliError::ProtocolMismatch { client, daemon, .. } = &error else {
                        return Err(error);
                    };
                    if *client != tilo_vm_protocol::PROTOCOL_VERSION
                        || daemon.checked_add(1) != Some(*client)
                    {
                        return Err(invalid("unexpected protocol mismatch versions"));
                    }
                    "speaks protocol"
                }
                "cas" => "fixture generation changed",
                "hash" => "imported userspace member differs",
                "foreign" => "architecture differs from the target",
                "corrupt" => "cohort",
                _ => return Err(invalid("unknown fixture mode")),
            };
            if !error.to_string().contains(reason) {
                return Err(invalid(&format!("{mode}: {error}")));
            }
        }
        Ok(true)
    }

    #[test]
    fn prepared_swap_uses_normal_transport_without_builder_and_preserves_cas() {
        if run_prepared_swap_child().unwrap() {
            return;
        }
        let fixture = Fixture::with_native_payload().unwrap();
        let output = fixture.run("batch", None).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest = Manifest::load(&fixture.root.path().join("os/components.toml"))
            .unwrap()
            .unwrap();
        let mut capture = Capture::new(fixture.root.path()).unwrap();
        let members = capture
            .receive(
                output.stdout.as_slice(),
                &manifest,
                &fixture.snapshot,
                std::env::consts::ARCH,
            )
            .unwrap();
        let path = capture
            .publish(
                &fixture.snapshot,
                std::env::consts::ARCH,
                members,
                serde_json::json!({"packages":[]}),
            )
            .unwrap();
        let peer = fixture.root.path().join("target/fixture-ssh");
        std::fs::write(&peer, APPLY_PEER).unwrap();
        std::fs::set_permissions(&peer, std::fs::Permissions::from_mode(0o755)).unwrap();
        // No builder configured: success proves this route never admits one.
        std::fs::write(
            fixture.root.path().join("target/vms.toml"),
            "[vms.fixture]\nhypervisor = \"utm\"\nssh = \"root@127.0.0.1\"\n",
        )
        .unwrap();
        let original = std::fs::read(path.join("cohort.stream")).unwrap();
        for mode in ["success", "foreign", "hash", "cas", "protocol", "corrupt"] {
            let calls = fixture.root.path().join("target/calls");
            for item in [&calls, &fixture.root.path().join("target/import-count")] {
                if item.exists() {
                    std::fs::remove_file(item).unwrap();
                }
            }
            if mode == "corrupt" {
                std::fs::write(path.join("cohort.stream"), &original[..original.len() / 2])
                    .unwrap();
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .current_dir(fixture.root.path())
                .args([
                    "app::userspace_prepare::store::tests::prepared_swap_uses_normal_transport_without_builder_and_preserves_cas",
                    "--exact",
                    "--nocapture",
                ])
                .env("TILO_VM_SSH", &peer)
                .env("TILO_PREPARED_FIXTURE_ROOT", fixture.root.path())
                .env("TILO_PREPARED_FIXTURE_PATH", &path)
                .env("TILO_PREPARED_FIXTURE_MODE", mode)
                .env(
                    "TILO_PREPARED_FIXTURE_PROTOCOL",
                    tilo_vm_protocol::PROTOCOL_VERSION.to_string(),
                )
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "child must actually run"
            );
            let requests: Vec<serde_json::Value> = if calls.exists() {
                std::fs::read_to_string(calls)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect()
            } else {
                vec![]
            };
            assert_fixture_request_order(mode, &requests);
        }
    }

    fn assert_fixture_request_order(mode: &str, requests: &[serde_json::Value]) {
        let count = |method| {
            requests
                .iter()
                .filter(|request| request["method"] == method)
                .count()
        };
        match mode {
            "success" | "cas" => {
                assert_eq!(count("layer.import"), 9);
                assert_eq!(count("userspace.apply"), 1);
            }
            "protocol" => {
                assert_eq!(requests.len(), 1, "mismatched peer receives only hello");
                assert_eq!(count("hello"), 1);
                assert_eq!(count("layer.import"), 0);
                assert_eq!(count("userspace.apply"), 0);
            }
            "foreign" => {
                assert_eq!(count("layer.upload.begin"), 0);
                assert_eq!(count("userspace.apply"), 0);
            }
            "hash" => {
                assert_eq!(count("layer.import"), 1);
                assert_eq!(count("userspace.apply"), 0);
            }
            _ => {
                assert_eq!(mode, "corrupt", "unknown fixture mode");
                assert!(
                    requests.is_empty(),
                    "corrupt payload must fail before contact"
                );
            }
        }
        assert_eq!(count("swap"), 0);
    }
}
