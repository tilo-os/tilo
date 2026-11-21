//! `apply-incoming`: the prebuilt component layers an installation left for a
//! machine's first boot, imported and swapped through the machine's own
//! daemon with no SSH.

use std::path::Path;

use tilo_vm_protocol::manifest::Scope;
use tilo_vm_protocol::{DEVICE_ACCOUNT, Swap, VM_ACCOUNT};

use super::{App, Outcome, local_layer};
use crate::cli::LocalLayerImport;
use crate::error::CliError;
use crate::incoming::Entry;
use crate::remote::Remote;

/// One component applied to an account, as the retry log shows it.
#[derive(Debug)]
pub(super) struct Applied {
    pub component: String,
    pub layer: String,
    pub account: String,
}

/// The account a component's scope merges into on a machine that runs no
/// named accounts: `vm` for the VM's own `/usr`, `device` for the machine's
/// own session, which tilo-vmd merges VM-wide.
fn account_for(scope: Scope) -> Result<&'static str, CliError> {
    match scope {
        Scope::Vm => Ok(VM_ACCOUNT),
        Scope::Session => Ok(DEVICE_ACCOUNT),
        Scope::App => Err(CliError::InvalidInput(
            "incoming layers carry components, not apps".into(),
        )),
    }
}

/// Import each layer and swap it into its account, in the order the caller
/// planned, removing each pair once it applied. Each applied component is
/// printed as it lands, so a later failure still shows what did. The first
/// failure stops the run with everything not yet applied left in place.
pub(super) fn apply(
    remote: &mut Remote,
    manifest: &Path,
    entries: &[Entry],
    announce: bool,
) -> Result<Vec<Applied>, CliError> {
    let expected_target = format!("{}-unknown-linux-gnu", remote.hello.arch);
    let mut applied = Vec::with_capacity(entries.len());
    for entry in entries {
        let account = account_for(entry.scope)?;
        let request = LocalLayerImport {
            component: entry.component.clone(),
            archive: entry.archive.clone(),
            archive_sha256: crate::incoming::archive_sha256(&entry.archive)?,
            provenance: entry.provenance.clone(),
            manifest: manifest.to_path_buf(),
        };
        let mut prepared = crate::local_layer::prepare(&request)?;
        if prepared.provenance.target.as_deref() != Some(expected_target.as_str()) {
            return Err(CliError::InvalidInput(format!(
                "incoming layer {} is for {}, but this machine is {expected_target}",
                entry.component,
                prepared
                    .provenance
                    .target
                    .as_deref()
                    .unwrap_or("an unknown target"),
            )));
        }
        let (hash, _existed) = local_layer::import(remote, &mut prepared)?;
        if entry.component == "vmd" {
            *remote = super::daemon::handoff_local(remote, &hash)?;
        }
        remote.call(Swap {
            account: account.into(),
            component: entry.component.clone(),
            layer: hash.clone(),
            info: Some(entry.info.clone()),
            partner: None,
        })?;
        crate::incoming::remove_applied(entry)?;
        let done = Applied {
            component: entry.component.clone(),
            layer: hash,
            account: account.into(),
        };
        if announce {
            println!("applied {} {} {}", done.component, done.layer, done.account);
        }
        applied.push(done);
    }
    Ok(applied)
}

impl App {
    pub(super) fn apply_incoming(
        &mut self,
        dir: &Path,
        manifest: &Path,
    ) -> Result<Outcome, CliError> {
        if !self.local {
            return Err(CliError::InvalidInput(
                "apply-incoming applies this machine's own incoming layers; pass --local".into(),
            ));
        }
        let entries = crate::incoming::entries(dir, manifest)?;
        self.receipt.set("incoming", dir.display().to_string());
        if entries.is_empty() {
            return Ok(Outcome::new(
                format!("no incoming layers in {}", dir.display()),
                serde_json::json!({ "directory": dir.display().to_string(), "components": [] }),
            ));
        }
        let mut remote = self.connect_here()?;
        let applied = self.receipt.phase("incoming", || {
            apply(&mut remote, manifest, &entries, !self.json)
        })?;
        Ok(Outcome::new(
            String::new(),
            serde_json::json!({
                "directory": dir.display().to_string(),
                "components": applied.iter().map(|entry| serde_json::json!({
                    "component": entry.component,
                    "layer": entry.layer,
                    "account": entry.account,
                })).collect::<Vec<_>>(),
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tilo_vm_protocol::Provenance;

    use super::*;
    use crate::hypervisor::CommandLine;

    /// The files an incoming pair carries, as the component installer lays
    /// them out; returns the layer hash the prepared archive hashes to.
    fn layer(dir: &Path, component: &str) -> (String, Provenance) {
        let tree = tempfile::tempdir().unwrap();
        let binary = tree.path().join("usr/bin").join(component);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&binary).unwrap();
        file.write_all(component.as_bytes()).unwrap();
        drop(file);
        let hash = tilo_vm_protocol::layer_hash::tree_hash(tree.path()).unwrap();
        let mut archive = std::fs::File::create(dir.join(format!("{component}.tar"))).unwrap();
        crate::build::pack_into(tree.path(), &mut archive).unwrap();
        drop(archive);
        let mut provenance = Provenance::local(component);
        provenance.source_commit = Some("a".repeat(40));
        provenance.cargo_lock_hash = Some("b".repeat(64));
        provenance.toolchain = Some("rustc 1.97.1".into());
        provenance.target = Some("x86_64-unknown-linux-gnu".into());
        std::fs::write(
            dir.join(format!("{component}.json")),
            serde_json::to_vec(&provenance).unwrap(),
        )
        .unwrap();
        (hash, provenance)
    }

    fn manifest(dir: &Path, components: &[(&str, &str)]) -> std::path::PathBuf {
        use std::fmt::Write as _;
        let mut text = String::new();
        for (name, scope) in components {
            write!(
                text,
                "[[component]]\nname = \"{name}\"\nkind = \"userspace\"\nscope = \"{scope}\"\npackages = [\"{name}\"]\nbins = [\"{name}\"]\nfiles = [{{ from = \"{name}\", to = \"/usr/bin/{name}\", mode = \"0755\" }}]\n\n"
            )
            .unwrap();
        }
        let path = dir.join("components.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    /// A daemon on this machine's stdin/stdout: it answers the import and
    /// swap calls as tilo-vmd does, and logs each method it was asked.
    const DAEMON: &str = r#"
import hashlib,json,struct,sys
hashes,files,log=json.loads(sys.argv[1]),sys.argv[2],sys.argv[3]
def emit(request,result):
 print(json.dumps({"jsonrpc":"2.0","id":request['id'],"result":result}),flush=True)
def read_binary():
 data=sys.stdin.buffer
 received=0;digest=hashlib.sha256()
 while True:
  size=struct.unpack('>I',data.read(4))[0]
  if size==0:
   count=struct.unpack('>Q',data.read(8))[0];hashed=data.read(32)
   if count!=received or hashed!=digest.digest():raise RuntimeError('bad binary footer')
   return received
  if size==0xffffffff:raise RuntimeError('binary upload cancelled')
  chunk=data.read(size);digest.update(chunk);received+=len(chunk)
for line in iter(sys.stdin.buffer.readline,b''):
 request=json.loads(line);method=request['method']
 with open(log,'a') as file:file.write(method+'\n')
 if method=='hello':emit(request,{"protocol":request['params']['protocol'],"daemon_version":"test","root":"test","arch":"x86_64"})
 elif method=='layer.upload.binary':
  emit(request,{"upload":"test"})
  emit(request,{"received":read_binary()})
 elif method=='layer.import':emit(request,{"hash":hashes[request['params']['provenance']['component']],"existed":False})
 elif method=='layer.verify':
  component={v:k for k,v in hashes.items()}[request['params']['hash']]
  emit(request,{"hash":hashes[component],"provenance":json.loads(open(files+'/'+component+'.json').read())})
 elif method=='swap':emit(request,{"account":request['params']['account'],"generation":1,"changed":True,"policy":"vm_refresh","actions":[],"warnings":[]})
 else:raise RuntimeError(method)
"#;

    fn connect(dir: &Path, hashes: &serde_json::Value) -> (Remote, std::path::PathBuf) {
        let log = dir.join("calls");
        let line = CommandLine::new("python3", &["-c", DAEMON])
            .arg(serde_json::to_string(hashes).unwrap())
            .arg(dir.join("incoming").display().to_string())
            .arg(log.display().to_string());
        (Remote::connect_local(&line).unwrap(), log)
    }

    #[test]
    fn the_local_surface_is_global_and_defaults_to_the_installed_paths() {
        use clap::Parser as _;

        use crate::cli::{Cli, Command};
        let parsed = Cli::try_parse_from(["tilo-vm", "--local", "apply-incoming"]).unwrap();
        assert!(parsed.local);
        let Some(Command::ApplyIncoming { dir, manifest }) = parsed.command else {
            panic!("apply-incoming did not reach its command");
        };
        assert_eq!(dir, std::path::Path::new(crate::incoming::INCOMING_DIR));
        assert_eq!(
            manifest,
            std::path::Path::new(crate::incoming::INSTALLED_MANIFEST)
        );
        // The daemon's own path is the vmd component's install path.
        assert_eq!(crate::remote::LOCAL_DAEMON, "/usr/libexec/tilo/tilo-vmd");
        let parsed = Cli::try_parse_from([
            "tilo-vm",
            "--local",
            "swap",
            "server",
            "--account",
            "vm",
            "--from",
            &format!("layer:{}", "a".repeat(64)),
        ])
        .unwrap();
        assert!(matches!(parsed.command, Some(Command::Swap { .. })));
    }

    #[test]
    fn incoming_layers_apply_in_manifest_order_into_the_scope_account_and_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let incoming = dir.path().join("incoming");
        std::fs::create_dir(&incoming).unwrap();
        let (zulu, _) = layer(&incoming, "zulu");
        let (alpha, _) = layer(&incoming, "alpha");
        let manifest = manifest(dir.path(), &[("zulu", "session"), ("alpha", "vm")]);
        let entries = crate::incoming::entries(&incoming, &manifest).unwrap();
        let hashes = serde_json::json!({ "zulu": zulu, "alpha": alpha });
        let (mut remote, log) = connect(dir.path(), &hashes);
        let applied = apply(&mut remote, &manifest, &entries, false).unwrap();
        assert_eq!(
            applied
                .iter()
                .map(|entry| (entry.component.as_str(), entry.account.as_str()))
                .collect::<Vec<_>>(),
            [("zulu", "device"), ("alpha", "vm")]
        );
        let calls = std::fs::read_to_string(&log).unwrap();
        let mut expected = String::from("hello\n");
        for _ in 0..2 {
            expected.push_str("layer.upload.binary\nlayer.import\nlayer.verify\nswap\n");
        }
        assert_eq!(calls, expected);
        assert!(std::fs::read_dir(&incoming).unwrap().next().is_none());
    }

    #[test]
    fn a_corrupt_layer_fails_before_the_daemon_imports_anything() {
        let dir = tempfile::tempdir().unwrap();
        let incoming = dir.path().join("incoming");
        std::fs::create_dir(&incoming).unwrap();
        let (alpha, _) = layer(&incoming, "alpha");
        // The archive no longer holds the declared file: the importer
        // refuses it instead of the boot continuing without it.
        std::fs::write(incoming.join("alpha.tar"), b"not a tar").unwrap();
        let manifest = manifest(dir.path(), &[("alpha", "vm")]);
        let entries = crate::incoming::entries(&incoming, &manifest).unwrap();
        let hashes = serde_json::json!({ "alpha": alpha });
        let (mut remote, log) = connect(dir.path(), &hashes);
        let error = apply(&mut remote, &manifest, &entries, false).unwrap_err();
        assert!(!error.to_string().is_empty());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "hello\n");
        assert!(incoming.join("alpha.tar").exists());
    }
}
